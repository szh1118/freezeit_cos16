use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use crate::{app::error::DaemonError, domain::runtime::RuntimeProcess};

pub const PROC_ROOT: &str = "/proc";
pub const CGROUP_ROOT: &str = "/sys/fs/cgroup";
pub const CGROUP_APPS_ROOT: &str = "/sys/fs/cgroup/apps";
pub const CGROUP_SYSTEM_ROOT: &str = "/sys/fs/cgroup/system";
const BINDER_DEBUG_PROC_ROOT: &str = "/sys/kernel/debug/binder/proc";
const BINDERFS_PROC_ROOT: &str = "/dev/binderfs/proc";
const SIGNAL_STOP_LEDGER_FILE: &str = ".freezeit-sigstop-ledger";
const SIGNAL_STOP_PROVENANCE_FILE: &str = ".freezeit-sigstop-provenance";

static SIGNAL_STOP_LEDGER_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

type SignalStopIdentity = (i32, u64);

/// Provenance of a Freezeit-owned SIGSTOP recorded in the persistent ledger.
///
/// A signal stop starts as `ResidualUnknown`: a generic freezer transaction may
/// already have changed cgroup or Binder state before it falls back to SIGSTOP.
/// Only the controller can promote a completed direct signal transaction to
/// `SignalOnly`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalStopOwnership {
    SignalOnly,
    ResidualUnknown,
}

/// Whether a main-ledger identity has the exact canonical two-field encoding.
/// Extended or duplicate records remain owned, but can never be promoted by a
/// sidecar entry because their provenance is not durable proof of a clean
/// signal-only transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignalStopLedgerRecord {
    Canonical,
    ResidualUnknown,
}

/// Identity of the atomically-replaced main ledger file. A rollback daemon
/// writes a new inode even when it writes the same two-field record, which
/// invalidates an older sidecar provenance claim on the next upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SignalStopLedgerGeneration {
    device: u64,
    inode: u64,
}

impl SignalStopLedgerGeneration {
    fn sidecar_token(self) -> String {
        format!("{}:{}", self.device, self.inode)
    }

    fn from_sidecar_token(token: &str) -> Option<Self> {
        let (device, inode) = token.split_once(':')?;
        if inode.contains(':') {
            return None;
        }
        Some(Self {
            device: device.parse().ok()?,
            inode: inode.parse().ok()?,
        })
    }
}

/// A sidecar record can only prove `SignalOnly` when it is one exact,
/// non-duplicated `pid start_time SignalOnly device:inode` line whose ledger
/// generation still matches. Invalid records are kept while reading solely to
/// force conservative recovery for that identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignalStopProvenanceRecord {
    SignalOnly(SignalStopLedgerGeneration),
    Invalid,
}

#[derive(Debug, Default)]
struct SignalStopProvenanceLedger {
    records: BTreeMap<SignalStopIdentity, SignalStopProvenanceRecord>,
    globally_untrusted: bool,
}

impl SignalStopProvenanceLedger {
    fn is_signal_only(
        &self,
        identity: SignalStopIdentity,
        generation: SignalStopLedgerGeneration,
    ) -> bool {
        !self.globally_untrusted
            && self.records.get(&identity)
                == Some(&SignalStopProvenanceRecord::SignalOnly(generation))
    }

    fn discard_untrusted_records(&mut self) {
        if self.globally_untrusted {
            self.records.clear();
            self.globally_untrusted = false;
        }
        self.records
            .retain(|_, record| matches!(record, SignalStopProvenanceRecord::SignalOnly(_)));
    }

    fn retain_current_generation(&mut self, generation: Option<SignalStopLedgerGeneration>) {
        self.discard_untrusted_records();
        self.records.retain(|_, record| {
            matches!(record, SignalStopProvenanceRecord::SignalOnly(record_generation) if Some(*record_generation) == generation)
        });
    }

    fn rebind_to_generation(&mut self, generation: SignalStopLedgerGeneration) {
        for record in self.records.values_mut() {
            if let SignalStopProvenanceRecord::SignalOnly(record_generation) = record {
                *record_generation = generation;
            }
        }
    }
}

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

/// 进程是否处于由本守护进程记录的 SIGSTOP 状态。
///
/// `/proc/<pid>/stat` 只能说明进程被停止，不能说明是谁停止的。仅凭 `T`/`t` 恢复会
/// 覆盖调试器、job-control 或其他 root 工具的决定，因此必须匹配持久化的 PID/start-time
/// 记录；tracing stop (`t`) 从不视为 Freezeit 所有。
pub fn proc_state_is_stopped(stat_text: &str) -> bool {
    freezeit_signal_stop_ownership(stat_text).is_some()
}

/// Return the durable provenance for a Freezeit-owned stopped process.
///
/// `/proc/<pid>/stat` provides the current PID, state, and start time.  The
/// entry is owned only when it is a job-control stop (`T`) and the same
/// `(pid, start_time)` exists in the SIGSTOP ledger; PID reuse and tracing
/// stops are therefore never recovered as Freezeit-owned stops.
pub fn freezeit_signal_stop_ownership(stat_text: &str) -> Option<SignalStopOwnership> {
    signal_stop_ownership_from_ledger(stat_text, &signal_stop_ledger_path())
}

/// Read the ledger provenance for an exact process identity without treating it
/// as evidence that the process is currently stopped.  The controller uses this
/// only before a new direct SIGSTOP transaction: a pre-existing residual record
/// must never be upgraded after a daemon restart lost its in-memory ownership.
pub fn recorded_freezeit_signal_stop_ownership(
    pid: i32,
    start_time_ticks: u64,
) -> Result<Option<SignalStopOwnership>, DaemonError> {
    recorded_signal_stop_ownership_from_ledger(&signal_stop_ledger_path(), pid, start_time_ticks)
}

pub fn read_proc_state_char(proc_root: impl AsRef<Path>, pid: i32) -> Result<char, DaemonError> {
    let stat = fs::read_to_string(proc_root.as_ref().join(pid.to_string()).join("stat"))?;
    parse_proc_state_char(&stat)
}

pub fn record_freezeit_signal_stop(pid: i32) -> Result<(), DaemonError> {
    let start_time_ticks = read_process_start_time(PROC_ROOT, pid)?;
    record_freezeit_signal_stop_at_path(&signal_stop_ledger_path(), pid, start_time_ticks)
}

fn record_freezeit_signal_stop_at_path(
    ledger_path: &Path,
    pid: i32,
    start_time_ticks: u64,
) -> Result<(), DaemonError> {
    with_signal_stop_ledger_lock(|| {
        let identity = (pid, start_time_ticks);
        let mut records = read_signal_stop_ledger(ledger_path)?;
        let provenance_path = signal_stop_provenance_path(ledger_path);
        let mut provenance = read_signal_stop_provenance(&provenance_path);

        provenance.retain_current_generation(signal_stop_ledger_generation(ledger_path).ok());
        records.insert(identity, SignalStopLedgerRecord::Canonical);
        retain_canonical_main_provenance(&records, &mut provenance);
        provenance.records.remove(&identity);

        rewrite_main_ledger_with_rebound_provenance(
            ledger_path,
            &records,
            &provenance_path,
            &mut provenance,
        )
    })
}

/// Promote an exact SIGSTOP ledger record after a controller-confirmed direct
/// signal transaction.  This never creates a new record: `SIGSTOP` itself must
/// have created the default residual record first.  `false` means the process
/// exited or changed identity before its durable record could be promoted.
pub fn promote_freezeit_signal_stop(pid: i32, start_time_ticks: u64) -> Result<bool, DaemonError> {
    promote_freezeit_signal_stop_at_path(&signal_stop_ledger_path(), pid, start_time_ticks)
}

fn promote_freezeit_signal_stop_at_path(
    ledger_path: &Path,
    pid: i32,
    start_time_ticks: u64,
) -> Result<bool, DaemonError> {
    with_signal_stop_ledger_lock(|| {
        let identity = (pid, start_time_ticks);
        let records = read_signal_stop_ledger(ledger_path)?;
        if records.get(&identity) != Some(&SignalStopLedgerRecord::Canonical) {
            return Ok(false);
        }
        let generation = match signal_stop_ledger_generation(ledger_path) {
            Ok(generation) => generation,
            Err(_) => return Ok(false),
        };
        let provenance_path = signal_stop_provenance_path(ledger_path);
        let mut provenance = read_signal_stop_provenance(&provenance_path);
        if provenance.globally_untrusted {
            return Ok(false);
        }
        match provenance.records.get(&identity) {
            Some(SignalStopProvenanceRecord::Invalid) => return Ok(false),
            Some(SignalStopProvenanceRecord::SignalOnly(record_generation))
                if *record_generation != generation =>
            {
                return Ok(false);
            }
            _ => {}
        }

        provenance.retain_current_generation(Some(generation));
        retain_canonical_main_provenance(&records, &mut provenance);
        provenance
            .records
            .insert(identity, SignalStopProvenanceRecord::SignalOnly(generation));
        write_signal_stop_provenance(&provenance_path, &provenance.records)?;
        Ok(true)
    })
}

pub fn clear_freezeit_signal_stop(pid: i32) -> Result<(), DaemonError> {
    let _ = take_freezeit_signal_stop(pid)?;
    Ok(())
}

pub fn take_freezeit_signal_stop(pid: i32) -> Result<bool, DaemonError> {
    let start_time_ticks = read_process_start_time(PROC_ROOT, pid)?;
    take_freezeit_signal_stop_at_path(&signal_stop_ledger_path(), pid, start_time_ticks)
}

fn take_freezeit_signal_stop_at_path(
    ledger_path: &Path,
    pid: i32,
    start_time_ticks: u64,
) -> Result<bool, DaemonError> {
    with_signal_stop_ledger_lock(|| {
        let identity = (pid, start_time_ticks);
        let mut records = read_signal_stop_ledger(ledger_path)?;
        let provenance_path = signal_stop_provenance_path(ledger_path);
        let mut provenance = read_signal_stop_provenance(&provenance_path);
        let removed = records.remove(&identity).is_some();

        provenance.retain_current_generation(signal_stop_ledger_generation(ledger_path).ok());
        retain_canonical_main_provenance(&records, &mut provenance);
        provenance.records.remove(&identity);

        rewrite_main_ledger_with_rebound_provenance(
            ledger_path,
            &records,
            &provenance_path,
            &mut provenance,
        )?;
        Ok(removed)
    })
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

    let actual_uid = match read_process_uid(proc_root.as_ref(), process.pid) {
        Ok(uid) => uid,
        Err(error) if is_disappeared_process_error(&error) => return Ok(false),
        Err(error) => return Err(error),
    };
    if actual_uid != process.uid {
        return Ok(false);
    }
    let cmdline = match read_process_cmdline(proc_root.as_ref(), process.pid) {
        Ok(cmdline) => cmdline,
        Err(error) if is_disappeared_process_error(&error) => return Ok(false),
        Err(error) => return Err(error),
    };
    if cmdline != process.process_name
        || !(cmdline == process.package_name
            || cmdline.starts_with(&format!("{}:", process.package_name)))
    {
        return Ok(false);
    }
    match process.start_time_ticks {
        Some(expected) => match read_process_start_time(proc_root, process.pid) {
            Ok(actual) => Ok(actual == expected),
            Err(error) if is_disappeared_process_error(&error) => Ok(false),
            Err(error) => Err(error),
        },
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
                binder_state: Some(binder_control_evidence(pid, &status_text)),
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
    discover_uid_processes_with_cgroup_roots(proc_root, &runtime_cgroup_roots(), uid)
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
            binder_state: Some(binder_control_evidence(pid, &status_text)),
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
        &runtime_cgroup_roots(),
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
                binder_state: Some(binder_control_evidence(pid, &status_text)),
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

fn runtime_cgroup_roots() -> [&'static Path; 3] {
    [
        Path::new(CGROUP_APPS_ROOT),
        Path::new(CGROUP_SYSTEM_ROOT),
        Path::new(CGROUP_ROOT),
    ]
}

fn is_disappeared_process_error(error: &DaemonError) -> bool {
    matches!(
        error,
        DaemonError::Io(io_error) if io_error.kind() == std::io::ErrorKind::NotFound
    )
}

fn binder_transaction_evidence(pid: i32) -> Option<String> {
    let roots = [
        Path::new(BINDER_DEBUG_PROC_ROOT),
        Path::new(BINDERFS_PROC_ROOT),
    ];
    roots
        .iter()
        .find_map(|root| fs::read_to_string(root.join(pid.to_string())).ok())
        .map(|snapshot| classify_binder_transaction_snapshot(&snapshot))
}

fn binder_control_evidence(pid: i32, status_text: &str) -> String {
    binder_transaction_evidence(pid).unwrap_or_else(|| {
        let context = process_context_switch_evidence(status_text)
            .unwrap_or_else(|| "context_switches unavailable".to_owned());
        format!("binder_queue unknown {context}")
    })
}

fn classify_binder_transaction_snapshot(snapshot: &str) -> String {
    let normalized = snapshot.to_ascii_lowercase();
    if normalized.contains("transaction stack")
        || normalized.contains("transaction_stack")
        || normalized.contains("sync_txn")
    {
        "binder_queue active_sync_transaction".to_owned()
    } else if normalized.contains("proc ") || normalized.contains("thread ") {
        "binder_queue idle".to_owned()
    } else {
        "binder_queue unknown".to_owned()
    }
}

fn signal_stop_ledger_path() -> PathBuf {
    let configured_module_dir = std::env::var(crate::config::loader::MODULE_DIR_ENV).ok();
    let module_dir = crate::config::loader::resolve_module_dir(
        std::env::args(),
        configured_module_dir.as_deref(),
    )
    .unwrap_or_else(|_| crate::config::loader::DEFAULT_MODULE_DIR.to_owned());
    PathBuf::from(module_dir).join(SIGNAL_STOP_LEDGER_FILE)
}

fn signal_stop_provenance_path(ledger_path: &Path) -> PathBuf {
    ledger_path.with_file_name(SIGNAL_STOP_PROVENANCE_FILE)
}

fn signal_stop_ledger_generation(
    ledger_path: &Path,
) -> Result<SignalStopLedgerGeneration, DaemonError> {
    let metadata = fs::metadata(ledger_path)?;
    Ok(SignalStopLedgerGeneration {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn signal_stop_ownership_from_ledger(
    stat_text: &str,
    ledger_path: &Path,
) -> Option<SignalStopOwnership> {
    if !matches!(parse_proc_state_char(stat_text), Ok('T')) {
        return None;
    }
    let pid = parse_process_pid(stat_text)?;
    let Ok(start_time_ticks) = parse_process_start_time(stat_text) else {
        return None;
    };
    recorded_signal_stop_ownership_from_ledger(ledger_path, pid, start_time_ticks)
        .ok()
        .flatten()
}

fn recorded_signal_stop_ownership_from_ledger(
    ledger_path: &Path,
    pid: i32,
    start_time_ticks: u64,
) -> Result<Option<SignalStopOwnership>, DaemonError> {
    let identity = (pid, start_time_ticks);
    let records = read_signal_stop_ledger(ledger_path)?;
    let Some(record) = records.get(&identity) else {
        return Ok(None);
    };

    let ownership = match (
        *record == SignalStopLedgerRecord::Canonical,
        signal_stop_ledger_generation(ledger_path).ok(),
    ) {
        (true, Some(generation))
            if read_signal_stop_provenance(&signal_stop_provenance_path(ledger_path))
                .is_signal_only(identity, generation) =>
        {
            SignalStopOwnership::SignalOnly
        }
        _ => SignalStopOwnership::ResidualUnknown,
    };
    Ok(Some(ownership))
}

fn parse_process_pid(stat_text: &str) -> Option<i32> {
    stat_text.split_whitespace().next()?.parse().ok()
}

fn with_signal_stop_ledger_lock<T>(
    operation: impl FnOnce() -> Result<T, DaemonError>,
) -> Result<T, DaemonError> {
    let lock = SIGNAL_STOP_LEDGER_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    operation()
}

fn read_signal_stop_ledger(
    ledger_path: &Path,
) -> Result<BTreeMap<SignalStopIdentity, SignalStopLedgerRecord>, DaemonError> {
    let text = match fs::read_to_string(ledger_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error.into()),
    };
    let mut records = BTreeMap::new();
    for line in text.lines() {
        let Some((pid, start_time_ticks, record)) = (|| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let start_time_ticks = fields.next()?.parse().ok()?;
            let record = if fields.next().is_some() {
                // Accept previously-written extended records as owned, but do
                // not let their obsolete in-band provenance survive migration.
                SignalStopLedgerRecord::ResidualUnknown
            } else {
                SignalStopLedgerRecord::Canonical
            };
            Some((pid, start_time_ticks, record))
        })() else {
            continue;
        };
        records
            .entry((pid, start_time_ticks))
            // Duplicate main identities cannot prove one complete transaction,
            // even if their sidecar happens to contain SignalOnly.
            .and_modify(|current| *current = SignalStopLedgerRecord::ResidualUnknown)
            .or_insert(record);
    }
    Ok(records)
}

fn read_signal_stop_provenance(provenance_path: &Path) -> SignalStopProvenanceLedger {
    let text = match fs::read_to_string(provenance_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return SignalStopProvenanceLedger::default();
        }
        Err(_) => {
            return SignalStopProvenanceLedger {
                records: BTreeMap::new(),
                globally_untrusted: true,
            };
        }
    };

    let mut provenance = SignalStopProvenanceLedger::default();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(start_time_ticks)) = (fields.next(), fields.next()) else {
            provenance.globally_untrusted = true;
            continue;
        };
        let (Ok(pid), Ok(start_time_ticks)) = (pid.parse(), start_time_ticks.parse()) else {
            provenance.globally_untrusted = true;
            continue;
        };
        let record = match (fields.next(), fields.next(), fields.next()) {
            (Some("SignalOnly"), Some(generation), None) => {
                SignalStopLedgerGeneration::from_sidecar_token(generation)
                    .map(SignalStopProvenanceRecord::SignalOnly)
                    .unwrap_or(SignalStopProvenanceRecord::Invalid)
            }
            _ => SignalStopProvenanceRecord::Invalid,
        };
        provenance
            .records
            .entry((pid, start_time_ticks))
            .and_modify(|current| *current = SignalStopProvenanceRecord::Invalid)
            .or_insert(record);
    }
    provenance
}

fn retain_canonical_main_provenance(
    records: &BTreeMap<SignalStopIdentity, SignalStopLedgerRecord>,
    provenance: &mut SignalStopProvenanceLedger,
) {
    provenance
        .records
        .retain(|identity, _| records.get(identity) == Some(&SignalStopLedgerRecord::Canonical));
}

fn rewrite_main_ledger_with_rebound_provenance(
    ledger_path: &Path,
    records: &BTreeMap<SignalStopIdentity, SignalStopLedgerRecord>,
    provenance_path: &Path,
    provenance: &mut SignalStopProvenanceLedger,
) -> Result<(), DaemonError> {
    // Replacing the main file changes its inode. Remove every sidecar claim
    // first so an interrupted write can only leave ResidualUnknown.
    write_signal_stop_ledger_file(provenance_path, Vec::new())?;
    write_signal_stop_ledger(ledger_path, records)?;
    if provenance.records.is_empty() {
        return Ok(());
    }

    let generation = signal_stop_ledger_generation(ledger_path)?;
    provenance.rebind_to_generation(generation);
    write_signal_stop_provenance(provenance_path, &provenance.records)
}

fn write_signal_stop_ledger(
    ledger_path: &Path,
    records: &BTreeMap<SignalStopIdentity, SignalStopLedgerRecord>,
) -> Result<(), DaemonError> {
    let lines = records
        .keys()
        .map(|(pid, start_time_ticks)| format!("{pid} {start_time_ticks}"))
        .collect();
    write_signal_stop_ledger_file(ledger_path, lines)
}

fn write_signal_stop_provenance(
    provenance_path: &Path,
    records: &BTreeMap<SignalStopIdentity, SignalStopProvenanceRecord>,
) -> Result<(), DaemonError> {
    let lines = records
        .iter()
        .filter_map(|((pid, start_time_ticks), record)| match record {
            SignalStopProvenanceRecord::SignalOnly(generation) => Some(format!(
                "{pid} {start_time_ticks} SignalOnly {}",
                generation.sidecar_token()
            )),
            SignalStopProvenanceRecord::Invalid => None,
        })
        .collect();
    write_signal_stop_ledger_file(provenance_path, lines)
}

fn write_signal_stop_ledger_file(file_path: &Path, lines: Vec<String>) -> Result<(), DaemonError> {
    if lines.is_empty() {
        match fs::remove_file(file_path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }

    let parent = file_path
        .parent()
        .ok_or_else(|| DaemonError::system("signal stop ledger has no parent directory"))?;
    fs::create_dir_all(parent)?;
    let temporary_path = file_path.with_extension(format!("tmp-{}", std::process::id()));
    let text = lines.join("\n");
    fs::write(&temporary_path, format!("{text}\n"))?;
    fs::rename(temporary_path, file_path)?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs, os::unix::fs::MetadataExt, path::Path};

    use super::*;

    #[test]
    fn identity_recheck_treats_a_disappeared_proc_entry_as_a_mismatch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let proc_root = temp.path().join("proc");
        let pid_dir = proc_root.join("123");
        fs::create_dir_all(&pid_dir).expect("pid directory");
        fs::write(
            pid_dir.join("status"),
            "Name:\texample\nUid:\t10123\t10123\t10123\t10123\n",
        )
        .expect("status");
        fs::write(pid_dir.join("cmdline"), "com.example.app\0").expect("cmdline");
        fs::write(
            pid_dir.join("stat"),
            "123 (example) S 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 4242\n",
        )
        .expect("stat");

        let process = discover_package_processes(&proc_root, "com.example.app", 10_123)
            .expect("discover")
            .remove(0);
        fs::remove_file(pid_dir.join("status")).expect("remove status");

        assert!(matches!(
            recheck_process_identity(&proc_root, &process),
            Ok(false)
        ));
    }

    #[test]
    fn runtime_discovery_checks_the_generic_cgroup_hierarchy_last() {
        assert_eq!(runtime_cgroup_roots()[2], Path::new(CGROUP_ROOT));
    }

    #[test]
    fn binder_debug_snapshot_distinguishes_active_transaction_from_idle_state() {
        assert_eq!(
            classify_binder_transaction_snapshot("proc 123\n  thread 123\n  transaction stack:\n"),
            "binder_queue active_sync_transaction"
        );
        assert_eq!(
            classify_binder_transaction_snapshot("proc 123\n  thread 123\n"),
            "binder_queue idle"
        );
    }

    #[test]
    fn stopped_process_without_a_freezeit_ledger_entry_is_not_owned() {
        assert!(!proc_state_is_stopped(
            "123 (example) T 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 4242\n"
        ));
    }

    #[test]
    fn matching_signal_stop_ledger_entry_is_required_for_restart_recovery() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ledger = temp.path().join("sigstop-ledger");
        fs::write(&ledger, "123 4242\n").expect("ledger");

        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 4242), &ledger),
            Some(SignalStopOwnership::ResidualUnknown)
        );
        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('t', 4242), &ledger),
            None
        );
        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 9999), &ledger),
            None
        );
    }

    #[test]
    fn legacy_signal_stop_ledger_entry_is_owned_but_residual_unknown() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ledger = temp.path().join("sigstop-ledger");
        fs::write(&ledger, "123 4242\n").expect("legacy ledger");

        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 4242), &ledger),
            Some(SignalStopOwnership::ResidualUnknown)
        );
    }

    #[test]
    fn signal_only_sidecar_entry_is_recognized() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ledger = temp.path().join("sigstop-ledger");
        let provenance = signal_stop_provenance_path_for_test(&ledger);
        fs::write(&ledger, "123 4242\n").expect("ledger");
        fs::write(
            &provenance,
            format!("{}\n", signal_only_provenance_line(&ledger, 123, 4242)),
        )
        .expect("signal-only provenance");

        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 4242), &ledger),
            Some(SignalStopOwnership::SignalOnly)
        );
    }

    #[test]
    fn unbound_signal_only_sidecar_entry_is_residual_unknown() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ledger = temp.path().join("sigstop-ledger");
        let provenance = signal_stop_provenance_path_for_test(&ledger);
        fs::write(&ledger, "123 4242\n").expect("ledger");
        fs::write(&provenance, "123 4242 SignalOnly\n").expect("unbound provenance");

        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 4242), &ledger),
            Some(SignalStopOwnership::ResidualUnknown)
        );
    }

    #[test]
    fn duplicate_signal_stop_ledger_entries_retain_residual_unknown_provenance() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ledger = temp.path().join("sigstop-ledger");
        let provenance = signal_stop_provenance_path_for_test(&ledger);
        fs::write(&ledger, "123 4242\n123 4242\n").expect("duplicate ledger");
        fs::write(
            &provenance,
            format!("{}\n", signal_only_provenance_line(&ledger, 123, 4242)),
        )
        .expect("provenance");

        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 4242), &ledger),
            Some(SignalStopOwnership::ResidualUnknown)
        );
    }

    #[test]
    fn duplicate_signal_only_sidecar_entries_are_residual_unknown() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ledger = temp.path().join("sigstop-ledger");
        let provenance = signal_stop_provenance_path_for_test(&ledger);
        fs::write(&ledger, "123 4242\n").expect("ledger");
        let record = signal_only_provenance_line(&ledger, 123, 4242);
        fs::write(&provenance, format!("{record}\n{record}\n"))
            .expect("duplicate signal-only provenance");

        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 4242), &ledger),
            Some(SignalStopOwnership::ResidualUnknown)
        );
    }

    #[test]
    fn malformed_signal_only_sidecar_entry_with_extra_fields_is_residual_unknown() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ledger = temp.path().join("sigstop-ledger");
        let provenance = signal_stop_provenance_path_for_test(&ledger);
        fs::write(&ledger, "123 4242\n").expect("ledger");
        fs::write(
            &provenance,
            format!(
                "{} unexpected\n",
                signal_only_provenance_line(&ledger, 123, 4242)
            ),
        )
        .expect("malformed provenance");

        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 4242), &ledger),
            Some(SignalStopOwnership::ResidualUnknown)
        );
    }

    #[test]
    fn new_signal_stop_ledger_records_are_written_in_the_legacy_two_field_format() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ledger = temp.path().join("sigstop-ledger");

        record_freezeit_signal_stop_at_path(&ledger, 123, 4242).expect("record stop");

        let text = fs::read_to_string(&ledger).expect("ledger text");
        assert!(text
            .lines()
            .all(|line| line.split_whitespace().count() == 2));
        assert_eq!(
            legacy_signal_stop_ledger_records(&text),
            BTreeSet::from([(123, 4242)])
        );
        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 4242), &ledger),
            Some(SignalStopOwnership::ResidualUnknown)
        );
    }

    #[test]
    fn promotion_requires_an_exact_existing_main_ledger_identity_and_writes_sidecar_provenance() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ledger = temp.path().join("sigstop-ledger");
        let provenance = signal_stop_provenance_path_for_test(&ledger);
        record_freezeit_signal_stop_at_path(&ledger, 123, 4242).expect("record stop");

        assert!(!promote_freezeit_signal_stop_at_path(&ledger, 123, 9999)
            .expect("non-matching promotion"));
        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 4242), &ledger),
            Some(SignalStopOwnership::ResidualUnknown)
        );
        assert!(
            promote_freezeit_signal_stop_at_path(&ledger, 123, 4242).expect("matching promotion")
        );
        assert_eq!(fs::read_to_string(&ledger).expect("ledger"), "123 4242\n");
        assert_eq!(
            fs::read_to_string(&provenance).expect("provenance"),
            format!("{}\n", signal_only_provenance_line(&ledger, 123, 4242))
        );
        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 4242), &ledger),
            Some(SignalStopOwnership::SignalOnly)
        );
    }

    #[test]
    fn recording_a_signal_stop_resets_matching_signal_only_provenance() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ledger = temp.path().join("sigstop-ledger");
        let provenance = signal_stop_provenance_path_for_test(&ledger);
        fs::write(&ledger, "123 4242\n").expect("ledger");
        fs::write(
            &provenance,
            format!("{}\n", signal_only_provenance_line(&ledger, 123, 4242)),
        )
        .expect("provenance");

        record_freezeit_signal_stop_at_path(&ledger, 123, 4242).expect("record stop");

        assert_eq!(fs::read_to_string(&ledger).expect("ledger"), "123 4242\n");
        assert!(!provenance.exists());
        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 4242), &ledger),
            Some(SignalStopOwnership::ResidualUnknown)
        );
    }

    #[test]
    fn taking_signal_stop_ledger_record_removes_matching_main_and_sidecar_records() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ledger = temp.path().join("sigstop-ledger");
        let provenance = signal_stop_provenance_path_for_test(&ledger);
        fs::write(&ledger, "123 4242\n123 9999\n").expect("ledger");
        fs::write(
            &provenance,
            format!(
                "{}\n{}\n",
                signal_only_provenance_line(&ledger, 123, 4242),
                signal_only_provenance_line(&ledger, 123, 9999)
            ),
        )
        .expect("provenance");

        assert!(
            take_freezeit_signal_stop_at_path(&ledger, 123, 4242).expect("remove matching record")
        );
        assert_eq!(fs::read_to_string(&ledger).expect("ledger"), "123 9999\n");
        assert_eq!(
            fs::read_to_string(&provenance).expect("provenance"),
            format!("{}\n", signal_only_provenance_line(&ledger, 123, 9999))
        );
        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 4242), &ledger),
            None
        );
        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 9999), &ledger),
            Some(SignalStopOwnership::SignalOnly)
        );
    }

    #[test]
    fn mismatching_sidecar_provenance_is_residual_unknown() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ledger = temp.path().join("sigstop-ledger");
        let provenance = signal_stop_provenance_path_for_test(&ledger);
        fs::write(&ledger, "123 4242\n").expect("ledger");
        fs::write(
            &provenance,
            format!("{}\n", signal_only_provenance_line(&ledger, 123, 9999)),
        )
        .expect("provenance");

        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 4242), &ledger),
            Some(SignalStopOwnership::ResidualUnknown)
        );
    }

    #[test]
    fn unknown_sidecar_provenance_is_residual_unknown() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ledger = temp.path().join("sigstop-ledger");
        let provenance = signal_stop_provenance_path_for_test(&ledger);
        fs::write(&ledger, "123 4242\n").expect("ledger");
        fs::write(&provenance, "123 4242 Unknown\n").expect("provenance");

        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 4242), &ledger),
            Some(SignalStopOwnership::ResidualUnknown)
        );
    }

    #[test]
    fn unreadable_sidecar_provenance_is_residual_unknown() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ledger = temp.path().join("sigstop-ledger");
        let provenance = signal_stop_provenance_path_for_test(&ledger);
        fs::write(&ledger, "123 4242\n").expect("ledger");
        fs::create_dir(&provenance).expect("unreadable provenance directory");

        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 4242), &ledger),
            Some(SignalStopOwnership::ResidualUnknown)
        );
    }

    #[test]
    fn rollback_daemon_main_ledger_rewrite_invalidates_signal_only_sidecar() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ledger = temp.path().join("sigstop-ledger");
        let provenance = signal_stop_provenance_path_for_test(&ledger);
        fs::write(&ledger, "123 4242\n").expect("ledger");
        fs::write(
            &provenance,
            format!("{}\n", signal_only_provenance_line(&ledger, 123, 4242)),
        )
        .expect("provenance");
        let original_inode = fs::metadata(&ledger).expect("original metadata").ino();

        legacy_rewrite_main_ledger(&ledger, "123 4242\n");

        assert_ne!(
            fs::metadata(&ledger).expect("rewritten metadata").ino(),
            original_inode
        );
        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 4242), &ledger),
            Some(SignalStopOwnership::ResidualUnknown)
        );
    }

    #[test]
    fn stale_sidecar_provenance_without_a_main_ledger_record_is_not_owned() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ledger = temp.path().join("sigstop-ledger");
        let provenance = signal_stop_provenance_path_for_test(&ledger);
        fs::write(&provenance, "123 4242 SignalOnly 1:2\n").expect("provenance");

        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 4242), &ledger),
            None
        );
    }

    #[test]
    fn legacy_extended_main_ledger_provenance_is_residual_unknown() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ledger = temp.path().join("sigstop-ledger");
        fs::write(&ledger, "123 4242 SignalOnly\n").expect("legacy extended ledger");

        assert_eq!(
            signal_stop_ownership_from_ledger(&stat_with_state('T', 4242), &ledger),
            Some(SignalStopOwnership::ResidualUnknown)
        );
    }

    fn signal_stop_provenance_path_for_test(ledger: &Path) -> std::path::PathBuf {
        ledger
            .parent()
            .expect("ledger parent")
            .join(".freezeit-sigstop-provenance")
    }

    fn signal_only_provenance_line(ledger: &Path, pid: i32, start_time_ticks: u64) -> String {
        let metadata = fs::metadata(ledger).expect("main ledger metadata");
        format!(
            "{pid} {start_time_ticks} SignalOnly {}:{}",
            metadata.dev(),
            metadata.ino()
        )
    }

    fn legacy_rewrite_main_ledger(ledger: &Path, text: &str) {
        let temporary = ledger.with_extension("legacy-tmp");
        fs::write(&temporary, text).expect("legacy temporary ledger");
        fs::rename(temporary, ledger).expect("legacy ledger rename");
    }

    fn legacy_signal_stop_ledger_records(text: &str) -> BTreeSet<(i32, u64)> {
        // Mirrors c718b72: any third main-ledger field makes the entire tail
        // non-numeric, so a rollback daemon would not own that SIGSTOP.
        text.lines()
            .filter_map(|line| {
                let (pid, start_time_ticks) = line.split_once(' ')?;
                Some((pid.parse().ok()?, start_time_ticks.parse().ok()?))
            })
            .collect()
    }

    fn stat_with_state(state: char, start_time_ticks: u64) -> String {
        let mut fields = vec!["0".to_owned(); 20];
        fields[0] = state.to_string();
        fields[19] = start_time_ticks.to_string();
        format!("123 (example) {}\n", fields.join(" "))
    }
}
