use std::{
    fs::OpenOptions,
    io,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
};

use crate::domain::capability::CapabilityStatus;

pub fn binder_device_candidates() -> [&'static str; 2] {
    ["/dev/binder", "/dev/binderfs/binder"]
}

pub fn discover_binder_device() -> Option<&'static str> {
    let capability = detect_binder_freezer_capability();
    if capability.status != CapabilityStatus::Available {
        return None;
    }

    binder_device_candidates().into_iter().find(|candidate| {
        capability
            .device_path
            .as_deref()
            .is_some_and(|path| path == *candidate)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinderFreezeRequest {
    Freeze,
    Unfreeze,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BinderFreezeInfo {
    pub pid: u32,
    pub enable: u32,
    pub timeout_ms: u32,
}

pub const BINDER_FREEZE_IOCTL: u64 = 0x400c_620e;

pub fn binder_freezer_ioctl_number() -> u64 {
    BINDER_FREEZE_IOCTL
}

pub fn binder_freezer_request(
    pid: u32,
    request: BinderFreezeRequest,
    timeout_ms: u32,
) -> BinderFreezeInfo {
    BinderFreezeInfo {
        pid,
        enable: u32::from(request == BinderFreezeRequest::Freeze),
        timeout_ms,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinderFreezerCapability {
    pub status: CapabilityStatus,
    pub device_path: Option<String>,
    pub evidence: String,
}

pub fn detect_binder_freezer_capability() -> BinderFreezerCapability {
    let candidates = binder_device_candidates()
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    detect_binder_freezer_capability_from_candidates(&candidates)
}

pub fn detect_binder_freezer_capability_from_candidates(
    candidates: &[PathBuf],
) -> BinderFreezerCapability {
    detect_binder_freezer_capability_with_probe(candidates, probe_binder_freezer)
}

pub fn detect_binder_freezer_capability_with_probe(
    candidates: &[PathBuf],
    mut probe: impl FnMut(&Path) -> io::Result<()>,
) -> BinderFreezerCapability {
    let mut attempted = Vec::new();

    for path in candidates.iter().filter(|candidate| candidate.exists()) {
        match probe(path) {
            Ok(()) => {
                return BinderFreezerCapability {
                    status: CapabilityStatus::Available,
                    device_path: Some(path.display().to_string()),
                    evidence: "binder freezer ioctl probe succeeded".to_owned(),
                };
            }
            // pid 0 is intentionally used for the capability probe. ESRCH confirms that the
            // kernel recognized BINDER_FREEZE while refusing to act on the sentinel PID.
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
                return BinderFreezerCapability {
                    status: CapabilityStatus::Available,
                    device_path: Some(path.display().to_string()),
                    evidence: format!("binder kernel recognized freezer ioctl: {error}"),
                };
            }
            Err(error) => attempted.push(format!("{}: {error}", path.display())),
        }
    }

    if attempted.is_empty() {
        return BinderFreezerCapability {
            status: CapabilityStatus::Missing,
            device_path: None,
            evidence: "no binder device found".to_owned(),
        };
    }

    BinderFreezerCapability {
        status: CapabilityStatus::Degraded,
        device_path: candidates
            .iter()
            .find(|candidate| candidate.exists())
            .map(|path| path.display().to_string()),
        evidence: format!(
            "binder freezer ioctl probe failed: {}",
            attempted.join("; ")
        ),
    }
}

fn probe_binder_freezer(path: &Path) -> io::Result<()> {
    set_binder_freeze(path, 0, BinderFreezeRequest::Freeze, 0)
}

pub fn set_binder_freeze(
    path: impl AsRef<Path>,
    pid: u32,
    request_kind: BinderFreezeRequest,
    timeout_ms: u32,
) -> io::Result<()> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let request_info = binder_freezer_request(pid, request_kind, timeout_ms);
    #[cfg(target_os = "android")]
    let request = binder_freezer_ioctl_number() as libc::c_int;
    #[cfg(not(target_os = "android"))]
    let request = binder_freezer_ioctl_number() as libc::c_ulong;
    let result = unsafe { libc::ioctl(file.as_raw_fd(), request, &request_info) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, io};

    use super::*;

    #[test]
    fn probe_tries_a_later_binder_device_after_the_first_one_is_unusable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("dev/binder");
        let second = temp.path().join("dev/binderfs/binder");
        fs::create_dir_all(first.parent().expect("first parent")).expect("first parent exists");
        fs::create_dir_all(second.parent().expect("second parent")).expect("second parent exists");
        fs::write(&first, "").expect("first device placeholder");
        fs::write(&second, "").expect("second device placeholder");

        let capability =
            detect_binder_freezer_capability_with_probe(&[first.clone(), second.clone()], |path| {
                if path == first {
                    Err(io::Error::from_raw_os_error(libc::EINVAL))
                } else {
                    Err(io::Error::from_raw_os_error(libc::ESRCH))
                }
            });

        assert_eq!(capability.status, CapabilityStatus::Available);
        assert_eq!(capability.device_path.as_deref(), second.to_str());
    }

    #[test]
    fn unsupported_or_denied_ioctl_is_not_reported_as_available() {
        let temp = tempfile::tempdir().expect("tempdir");
        let binder = temp.path().join("dev/binder");
        fs::create_dir_all(binder.parent().expect("binder parent")).expect("binder parent exists");
        fs::write(&binder, "").expect("binder device placeholder");

        for errno in [libc::EINVAL, libc::EPERM] {
            let capability = detect_binder_freezer_capability_with_probe(&[binder.clone()], |_| {
                Err(io::Error::from_raw_os_error(errno))
            });
            assert_ne!(capability.status, CapabilityStatus::Available);
        }
    }
}
