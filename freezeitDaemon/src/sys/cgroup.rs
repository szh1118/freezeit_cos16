use std::{
    fs::{self, OpenOptions},
    io::ErrorKind,
    path::{Path, PathBuf},
};

use crate::app::error::DaemonError;
use crate::domain::capability::CapabilityStatus;

pub const CGROUP_FREEZE_FILE: &str = "cgroup.freeze";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CgroupFreezerPreference {
    AndroidAppCgroupV2,
    SystemCgroupV2,
    GenericCgroupV2,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgroupFreezerCapability {
    pub status: CapabilityStatus,
    pub preference: CgroupFreezerPreference,
    pub freeze_files: Vec<PathBuf>,
    pub evidence: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreezeState {
    Thawed,
    Frozen,
}

impl FreezeState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Thawed => "0",
            Self::Frozen => "1",
        }
    }
}

pub fn discover_freeze_files(root: impl AsRef<Path>) -> Result<Vec<PathBuf>, DaemonError> {
    let root = root.as_ref();
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut paths = Vec::new();
    discover_freeze_files_inner(root, &mut paths)?;
    Ok(paths)
}

pub fn detect_cgroup_v2_freezer_capability(
    cgroup_root: impl AsRef<Path>,
) -> Result<CgroupFreezerCapability, DaemonError> {
    let cgroup_root = cgroup_root.as_ref();
    let controllers = cgroup_root.join("cgroup.controllers");
    let controllers_text = fs::read_to_string(&controllers).unwrap_or_default();
    let controller_has_freezer = controllers_text
        .split_whitespace()
        .any(|controller| controller == "freezer");

    // Scan once, then classify the discovered paths. Scanning apps, system, and the full root
    // recursively rewalks the Android-specific subtrees on every diagnostic refresh.
    let apps_root = cgroup_root.join("apps");
    let system_root = cgroup_root.join("system");
    let discovered_files = discover_freeze_files(cgroup_root)?;
    let app_files = discovered_files
        .iter()
        .filter(|path| path.starts_with(&apps_root))
        .cloned()
        .collect::<Vec<_>>();
    let system_files = discovered_files
        .iter()
        .filter(|path| path.starts_with(&system_root))
        .cloned()
        .collect::<Vec<_>>();
    let generic_files = discovered_files
        .into_iter()
        .filter(|path| !path.starts_with(&apps_root) && !path.starts_with(&system_root))
        .collect::<Vec<_>>();
    let (status, preference, freeze_files, discovered_count) =
        select_writable_freeze_files(app_files, system_files, generic_files);

    Ok(CgroupFreezerCapability {
        status,
        preference,
        evidence: format!(
            "{} contains freezer={controller_has_freezer}; freeze_files={}; writable_freeze_files={}",
            controllers.display(),
            discovered_count,
            freeze_files.len()
        ),
        freeze_files,
    })
}

fn select_writable_freeze_files(
    app_files: Vec<PathBuf>,
    system_files: Vec<PathBuf>,
    generic_files: Vec<PathBuf>,
) -> (
    CapabilityStatus,
    CgroupFreezerPreference,
    Vec<PathBuf>,
    usize,
) {
    let mut first_unwritable = None;
    for (preference, files) in [
        (CgroupFreezerPreference::AndroidAppCgroupV2, app_files),
        (CgroupFreezerPreference::SystemCgroupV2, system_files),
        (CgroupFreezerPreference::GenericCgroupV2, generic_files),
    ] {
        if files.is_empty() {
            continue;
        }
        let discovered_count = files.len();
        let writable_files = files
            .into_iter()
            .filter(|path| can_open_freeze_file_for_write(path))
            .collect::<Vec<_>>();
        if !writable_files.is_empty() {
            return (
                CapabilityStatus::Available,
                preference,
                writable_files,
                discovered_count,
            );
        }
        first_unwritable.get_or_insert((preference, discovered_count));
    }

    match first_unwritable {
        Some((preference, discovered_count)) => (
            CapabilityStatus::Degraded,
            preference,
            Vec::new(),
            discovered_count,
        ),
        None => (
            CapabilityStatus::Missing,
            CgroupFreezerPreference::Missing,
            Vec::new(),
            0,
        ),
    }
}

fn discover_freeze_files_inner(path: &Path, paths: &mut Vec<PathBuf>) -> Result<(), DaemonError> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let child_path = entry.path();
        if child_path
            .file_name()
            .is_some_and(|name| name == CGROUP_FREEZE_FILE)
        {
            paths.push(child_path);
        } else if child_path.is_dir() {
            discover_freeze_files_inner(&child_path, paths)?;
        }
    }
    Ok(())
}

fn can_open_freeze_file_for_write(path: &Path) -> bool {
    OpenOptions::new().write(true).open(path).is_ok()
}

pub fn read_freeze_state(path: impl AsRef<Path>) -> Result<FreezeState, DaemonError> {
    match fs::read_to_string(path)?.trim() {
        "0" => Ok(FreezeState::Thawed),
        "1" => Ok(FreezeState::Frozen),
        value => Err(DaemonError::system(format!(
            "unknown cgroup.freeze state {value}"
        ))),
    }
}

pub fn write_freeze_state(path: impl AsRef<Path>, state: FreezeState) -> Result<(), DaemonError> {
    fs::write(path, state.as_str())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_writable_discovered_freeze_node_is_not_available() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("cgroup");
        let invalid_freeze_node = root.join("uid_10123/pid_123/cgroup.freeze");
        fs::create_dir_all(&invalid_freeze_node).expect("create directory-shaped freeze node");

        let capability = detect_cgroup_v2_freezer_capability(&root).expect("detect capability");

        assert_eq!(capability.status, CapabilityStatus::Degraded);
    }

    #[test]
    fn writable_system_hierarchy_follows_an_unwritable_app_hierarchy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("cgroup");
        let app_freeze = root.join("apps/uid_10123/pid_123/cgroup.freeze");
        let system_freeze = root.join("system/uid_10123/pid_123/cgroup.freeze");
        fs::create_dir_all(&app_freeze).expect("directory-shaped app freeze node");
        fs::create_dir_all(system_freeze.parent().expect("system parent")).expect("system parent");
        fs::write(&system_freeze, "0").expect("system freeze node");

        let capability = detect_cgroup_v2_freezer_capability(&root).expect("detect capability");

        assert_eq!(capability.status, CapabilityStatus::Available);
        assert_eq!(
            capability.preference,
            CgroupFreezerPreference::SystemCgroupV2
        );
        assert_eq!(capability.freeze_files, vec![system_freeze]);
    }
}
