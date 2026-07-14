use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use crate::{app::error::DaemonError, domain::runtime::RuntimeProcess};

pub const PROC_ROOT: &str = "/proc";
pub const CGROUP_APPS_ROOT: &str = "/sys/fs/cgroup/apps";
pub const CGROUP_SYSTEM_ROOT: &str = "/sys/fs/cgroup/system";

pub fn pid_exists(proc_root: impl AsRef<Path>, pid: i32) -> bool {
    proc_root.as_ref().join(pid.to_string()).exists()
}

pub fn process_status_path(proc_root: impl AsRef<Path>, pid: i32) -> PathBuf {
    proc_root.as_ref().join(pid.to_string()).join("status")
}

pub fn read_uid_from_status(status_text: &str) -> Result<u32, DaemonError> {
    status_text
        .lines()
        .find_map(|line| {
            line.strip_prefix("Uid:")
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|value| value.parse::<u32>().ok())
        })
        .ok_or_else(|| DaemonError::system("proc status did not contain a readable Uid line"))
}

pub fn read_process_uid(proc_root: impl AsRef<Path>, pid: i32) -> Result<u32, DaemonError> {
    let status = fs::read_to_string(process_status_path(proc_root, pid))?;
    read_uid_from_status(&status)
}

pub fn read_process_cmdline(proc_root: impl AsRef<Path>, pid: i32) -> Result<String, DaemonError> {
    Ok(
        fs::read_to_string(proc_root.as_ref().join(pid.to_string()).join("cmdline"))?
            .split('\0')
            .next()
            .unwrap_or("")
            .to_owned(),
    )
}

pub fn parse_process_start_time(stat_text: &str) -> Result<u64, DaemonError> {
    let command_end = stat_text
        .rfind(')')
        .ok_or_else(|| DaemonError::system("proc stat did not contain a command terminator"))?;
    stat_text[command_end + 1..]
        .split_whitespace()
        .nth(19)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| DaemonError::system("proc stat did not contain a readable start time"))
}

pub fn read_process_start_time(proc_root: impl AsRef<Path>, pid: i32) -> Result<u64, DaemonError> {
    let stat = fs::read_to_string(proc_root.as_ref().join(pid.to_string()).join("stat"))?;
    parse_process_start_time(&stat)
}

/// 解析 /proc/<pid>/stat 的 state 字段（comm 右括号后的第一个字符）。
/// 'T' = 停止（job control/信号），'t' = tracing stop。两者都意味着进程被挂起，
/// 可能是上一次守护进程用 SIGSTOP 冻结后、daemon 在重启前未发 SIGCONT 恢复所致。
pub fn parse_proc_state_char(stat_text: &str) -> Result<char, DaemonError> {
    let command_end = stat_text
        .rfind(')')
        .ok_or_else(|| DaemonError::system("proc stat did not contain a command terminator"))?;
    stat_text[command_end + 1..]
        .split_whitespace()
        .next()
        .and_then(|token| token.chars().next())
        .ok_or_else(|| DaemonError::system("proc stat did not contain a readable state field"))
}

/// 进程是否处于被信号停止的状态（'T'/'t'），需要 SIGCONT 恢复。
pub fn proc_state_is_stopped(stat_text: &str) -> bool {
    matches!(parse_proc_state_char(stat_text), Ok('T') | Ok('t'))
}

pub fn read_proc_state_char(proc_root: impl AsRef<Path>, pid: i32) -> Result<char, DaemonError> {
    let stat = fs::read_to_string(proc_root.as_ref().join(pid.to_string()).join("stat"))?;
    parse_proc_state_char(&stat)
}

pub fn process_context_switch_evidence(status_text: &str) -> Option<String> {
    let voluntary = read_status_u64(status_text, "voluntary_ctxt_switches:")?;
    let nonvoluntary = read_status_u64(status_text, "nonvoluntary_ctxt_switches:")?;
    Some(format!(
        "context_switches voluntary={voluntary} nonvoluntary={nonvoluntary} total={}",
        voluntary + nonvoluntary
    ))
}

pub fn recheck_process_identity(
    proc_root: impl AsRef<Path>,
    process: &RuntimeProcess,
) -> Result<bool, DaemonError> {
    if !pid_exists(proc_root.as_ref(), process.pid) {
        return Ok(false);
    }

    if read_process_uid(proc_root.as_ref(), process.pid)? != process.uid {
        return Ok(false);
    }
    let cmdline = read_process_cmdline(proc_root.as_ref(), process.pid)?;
    if cmdline != process.process_name
        || !(cmdline == process.package_name
            || cmdline.starts_with(&format!("{}:", process.package_name)))
    {
        return Ok(false);
    }
    match process.start_time_ticks {
        Some(expected) => Ok(read_process_start_time(proc_root, process.pid)? == expected),
        None => Ok(false),
    }
}

pub fn discover_package_processes(
    proc_root: impl AsRef<Path>,
    package_name: &str,
    uid: u32,
) -> Result<Vec<RuntimeProcess>, DaemonError> {
    let proc_root = proc_root.as_ref();
    if !proc_root.exists() {
        return Ok(Vec::new());
    }

    let mut processes = Vec::new();
    for entry in fs::read_dir(proc_root)? {
        let entry = entry?;
        let Some(pid_text) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        let Ok(pid) = pid_text.parse::<i32>() else {
            continue;
        };
        let Ok(status_text) = fs::read_to_string(entry.path().join("status")) else {
            continue;
        };
        let Ok(process_uid) = read_uid_from_status(&status_text) else {
            continue;
        };
        if process_uid != uid {
            continue;
        }

        let process_name = fs::read_to_string(entry.path().join("cmdline"))
            .unwrap_or_default()
            .split('\0')
            .next()
            .unwrap_or("")
            .to_owned();

        if process_name == package_name || process_name.starts_with(&format!("{package_name}:")) {
            processes.push(RuntimeProcess {
                pid,
                uid,
                package_name: package_name.to_owned(),
                process_name,
                proc_state: crate::domain::runtime::ProcessState::Unknown,
                control_state: crate::domain::runtime::ControlState::Unknown,
                cgroup_freeze_path: None,
                binder_state: process_context_switch_evidence(&status_text),
                start_time_ticks: read_process_start_time(proc_root, pid).ok(),
                last_seen_at_ms: 0,
            });
        }
    }

    Ok(processes)
}

pub fn discover_uid_processes(
    proc_root: impl AsRef<Path>,
    uid: u32,
) -> Result<Vec<RuntimeProcess>, DaemonError> {
    discover_uid_processes_with_cgroup_roots(
        proc_root,
        &[Path::new(CGROUP_APPS_ROOT), Path::new(CGROUP_SYSTEM_ROOT)],
        uid,
    )
}

pub fn discover_uid_processes_with_cgroup_root(
    proc_root: impl AsRef<Path>,
    cgroup_root: impl AsRef<Path>,
    uid: u32,
) -> Result<Vec<RuntimeProcess>, DaemonError> {
    discover_uid_processes_with_cgroup_roots(proc_root, &[cgroup_root.as_ref()], uid)
}

pub fn discover_uid_processes_with_cgroup_roots(
    proc_root: impl AsRef<Path>,
    cgroup_roots: &[&Path],
    uid: u32,
) -> Result<Vec<RuntimeProcess>, DaemonError> {
    let proc_root = proc_root.as_ref();
    if !proc_root.exists() {
        return Ok(Vec::new());
    }

    let mut processes = Vec::new();
    for entry in fs::read_dir(proc_root)? {
        let entry = entry?;
        let Some(pid_text) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        let Ok(pid) = pid_text.parse::<i32>() else {
            continue;
        };
        let Ok(status_text) = fs::read_to_string(entry.path().join("status")) else {
            continue;
        };
        let Ok(process_uid) = read_uid_from_status(&status_text) else {
            continue;
        };
        if process_uid != uid {
            continue;
        }

        let process_name = fs::read_to_string(entry.path().join("cmdline"))
            .unwrap_or_default()
            .split('\0')
            .next()
            .unwrap_or("")
            .to_owned();
        if process_name.is_empty() {
            continue;
        }

        let package_name = process_name
            .split(':')
            .next()
            .unwrap_or(&process_name)
            .to_owned();

        processes.push(RuntimeProcess {
            pid,
            uid,
            package_name,
            process_name,
            proc_state: crate::domain::runtime::ProcessState::Cached,
            control_state: crate::domain::runtime::ControlState::Running,
            cgroup_freeze_path: cgroup_freeze_path(cgroup_roots, uid, pid),
            binder_state: process_context_switch_evidence(&status_text),
            start_time_ticks: read_process_start_time(proc_root, pid).ok(),
            last_seen_at_ms: 0,
        });
    }

    Ok(processes)
}

pub fn discover_managed_uid_processes(
    proc_root: impl AsRef<Path>,
    managed_uids: &BTreeSet<u32>,
) -> Result<BTreeMap<u32, Vec<RuntimeProcess>>, DaemonError> {
    discover_managed_uid_processes_with_cgroup_roots(
        proc_root,
        &[Path::new(CGROUP_APPS_ROOT), Path::new(CGROUP_SYSTEM_ROOT)],
        managed_uids,
    )
}

pub fn discover_managed_uid_processes_with_cgroup_roots(
    proc_root: impl AsRef<Path>,
    cgroup_roots: &[&Path],
    managed_uids: &BTreeSet<u32>,
) -> Result<BTreeMap<u32, Vec<RuntimeProcess>>, DaemonError> {
    let proc_root = proc_root.as_ref();
    if managed_uids.is_empty() || !proc_root.exists() {
        return Ok(BTreeMap::new());
    }

    let mut processes_by_uid: BTreeMap<u32, Vec<RuntimeProcess>> = BTreeMap::new();
    for entry in fs::read_dir(proc_root)? {
        let entry = entry?;
        let Some(pid_text) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        let Ok(pid) = pid_text.parse::<i32>() else {
            continue;
        };
        let Ok(status_text) = fs::read_to_string(entry.path().join("status")) else {
            continue;
        };
        let Ok(process_uid) = read_uid_from_status(&status_text) else {
            continue;
        };
        if !managed_uids.contains(&process_uid) {
            continue;
        }

        let process_name = fs::read_to_string(entry.path().join("cmdline"))
            .unwrap_or_default()
            .split('\0')
            .next()
            .unwrap_or("")
            .to_owned();
        if process_name.is_empty() {
            continue;
        }

        let package_name = process_name
            .split(':')
            .next()
            .unwrap_or(&process_name)
            .to_owned();

        processes_by_uid
            .entry(process_uid)
            .or_default()
            .push(RuntimeProcess {
                pid,
                uid: process_uid,
                package_name,
                process_name,
                proc_state: crate::domain::runtime::ProcessState::Cached,
                control_state: crate::domain::runtime::ControlState::Running,
                cgroup_freeze_path: cgroup_freeze_path(cgroup_roots, process_uid, pid),
                binder_state: process_context_switch_evidence(&status_text),
                start_time_ticks: read_process_start_time(proc_root, pid).ok(),
                last_seen_at_ms: 0,
            });
    }

    Ok(processes_by_uid)
}

fn read_status_u64(status_text: &str, field: &str) -> Option<u64> {
    status_text.lines().find_map(|line| {
        line.strip_prefix(field)
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|value| value.parse().ok())
    })
}

fn cgroup_freeze_path(cgroup_roots: &[&Path], uid: u32, pid: i32) -> Option<String> {
    cgroup_roots.iter().find_map(|cgroup_root| {
        let path = cgroup_root
            .join(format!("uid_{uid}"))
            .join(format!("pid_{pid}"))
            .join(crate::sys::cgroup::CGROUP_FREEZE_FILE);
        if path.exists() {
            path.to_str().map(ToOwned::to_owned)
        } else {
            None
        }
    })
}
