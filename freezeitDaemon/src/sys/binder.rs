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
    binder_device_candidates()
        .into_iter()
        .find(|candidate| Path::new(candidate).exists())
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
    let Some(path) = candidates.iter().find(|candidate| candidate.exists()) else {
        return BinderFreezerCapability {
            status: CapabilityStatus::Missing,
            device_path: None,
            evidence: "no binder device found".to_owned(),
        };
    };

    match probe(path) {
        Ok(()) => BinderFreezerCapability {
            status: CapabilityStatus::Available,
            device_path: Some(path.display().to_string()),
            evidence: "binder freezer ioctl probe succeeded".to_owned(),
        },
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(libc::ESRCH | libc::EINVAL | libc::EPERM)
            ) =>
        {
            BinderFreezerCapability {
                status: CapabilityStatus::Available,
                device_path: Some(path.display().to_string()),
                evidence: format!("binder kernel recognized freezer ioctl: {error}"),
            }
        }
        Err(error) => BinderFreezerCapability {
            status: CapabilityStatus::Degraded,
            device_path: Some(path.display().to_string()),
            evidence: format!("binder freezer ioctl probe failed: {error}"),
        },
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
