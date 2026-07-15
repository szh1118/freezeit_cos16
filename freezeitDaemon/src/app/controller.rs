use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    app::{
        command_runner::run_command,
        compatibility::{load_verified_targets, RuntimeEnvironment},
        download_deferral::{
            sample_uid_rx_bytes, DownloadDeferral, DownloadDeferralAction, UidRxSample,
            DOWNLOAD_RETRY_DELAY_MS, INITIAL_SAMPLE_DELAY_MS,
        },
        error::DaemonError,
        freezer_backend::{
            mark_processes_frozen, mark_processes_running, BackendEnvironment, DecisionAction,
            FreezeDecision, SystemAwareCgroupBinderBackend,
        },
        health::ModuleHealth,
        logging::{LogLevel, LogRecord},
        operation_log::OperationLog,
        package_inventory::{
            parse_cmd_package_list, protected_reason_for, reconcile_uid, PackageRecord,
        },
    },
    config::{
        loader::{load_policy_files, load_policy_files_recovering, DaemonPaths, LoadedPolicyFiles},
        migration::{parse_legacy_policy_line, parse_legacy_policy_target, LegacyPolicyTarget},
    },
    domain::{
        capability::ControlCapability,
        operation::{ControlAction, ControlOperation, OperationResult},
        policy::{
            FallbackAction, ForegroundStrategy, FreezeMode, FreezePolicy, ManagedApp,
            ProtectedReason,
        },
        runtime::{ControlState, RuntimeProcess},
    },
    protocol::{
        manager_v1::{
            encode_app_config, encode_xposed_config_payload, handle_read_only_command,
            normalize_settings, source_tokens_from_legacy_config, ManagerAppConfigRecord,
            ManagerCommand, ManagerFreezeStatusRecord, ReadOnlyState,
        },
        manager_v2::{
            capability_report_json, compatibility_report_json, health_report_json,
            operation_log_json, self_check_json, self_check_json_for_runtime,
        },
        xposed::{classify_bridge_error, classify_hook_health_payload},
    },
    sys::{binder, cgroup, procfs, signal, socket, xposed_bridge},
};

const CAPABILITY_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct CachedCapabilities {
    observed_at: Instant,
    values: Vec<ControlCapability>,
}

static RUNTIME_CAPABILITY_CACHE: OnceLock<Mutex<Option<CachedCapabilities>>> = OnceLock::new();

fn discover_runtime_capabilities() -> Vec<ControlCapability> {
    let cache = RUNTIME_CAPABILITY_CACHE.get_or_init(|| Mutex::new(None));
    let now = Instant::now();
    if let Ok(guard) = cache.lock() {
        if let Some(cached) = guard.as_ref() {
            if now.duration_since(cached.observed_at) < CAPABILITY_CACHE_TTL {
                return cached.values.clone();
            }
        }
    }

    let capabilities =
        SystemAwareCgroupBinderBackend::new(BackendEnvironment::detect()).discover_capabilities();
    if let Ok(mut guard) = cache.lock() {
        *guard = Some(CachedCapabilities {
            observed_at: now,
            values: capabilities.clone(),
        });
    }
    capabilities
}

pub fn run() -> Result<(), DaemonError> {
    run_with_paths(&DaemonPaths::from_module_dir(
        crate::config::loader::DEFAULT_MODULE_DIR,
    ))
}

pub fn run_with_paths(paths: &DaemonPaths) -> Result<(), DaemonError> {
    let mut state = startup_read_only_state_from_paths(paths);
    if let Err(error) = sync_loaded_config_to_hook(&mut state, xposed_bridge::set_config) {
        state.manager_log.push_once(LogRecord::fault(
            LogLevel::Error,
            current_timestamp_ms(),
            format!("hook config sync failed: {error}"),
        ));
    }
    // 在进入 server 主循环前，恢复上一轮用 SIGSTOP 冻结、daemon 重启后无人恢复的进程。
    recover_stopped_managed_processes_after_restart(&mut state);
    socket::run_manager_server_forever(state)
}

pub fn startup_read_only_state() -> ReadOnlyState {
    startup_read_only_state_from_paths(&DaemonPaths::from_module_dir(
        crate::config::loader::DEFAULT_MODULE_DIR,
    ))
}

pub fn startup_read_only_state_from_paths(paths: &DaemonPaths) -> ReadOnlyState {
    let mut state = ReadOnlyState {
        settings_path: Some(paths.settings_db.clone()),
        app_config_path: Some(paths.app_config.clone()),
        app_label_path: Some(paths.app_label.clone()),
        ..ReadOnlyState::default()
    };
    apply_module_prop(&mut state, &format!("{}/module.prop", paths.module_dir));
    state.changelog = read_first_existing_text(&[
        format!("{}/CHANGELOG.md", paths.module_dir),
        format!("{}/changelog.md", paths.module_dir),
        format!("{}/changelog.txt", paths.module_dir),
    ])
    .unwrap_or_default();
    if let Some(log) = read_first_existing_text(&[
        format!("{}/boot.log", paths.module_dir),
        format!("{}/freezeit.log", paths.module_dir),
        format!("{}/daemon.log", paths.module_dir),
    ]) {
        import_startup_log(&mut state, &sanitize_startup_log(&log));
    }
    let policy = load_policy_files_recovering(paths);
    let policy_ready = policy.is_available();
    state.settings = normalize_settings(policy.settings);
    let package_records = load_package_records();
    state.app_config =
        load_manager_app_config_records(policy.app_config.as_deref(), &package_records);
    state.app_config_source_tokens =
        source_tokens_from_legacy_config(policy.app_config.as_deref(), |package_name| {
            package_records
                .iter()
                .find(|record| record.package_name == package_name)
                .map(|record| record.uid)
        });
    let mut runtime = detect_runtime_environment();
    runtime.verified_targets =
        load_verified_targets(&paths.rom_baseline, &paths.verified_targets).unwrap_or_default();
    state.verified_targets = runtime
        .verified_targets
        .iter()
        .map(|target| (target.device_model.clone(), target.sdk))
        .collect();
    state.android_version = runtime.android_version.clone();
    state.kernel_version = runtime.kernel.clone();
    state.cluster_num = detect_cpu_cluster_count();
    state.ext_memory_mib = detect_ext_memory_mib();
    state.work_mode = "FreezerV2 / Rust daemon".to_owned();
    state.daemon_health = "active".to_owned();
    append_daemon_status_log(&mut state);
    let capabilities = discover_runtime_capabilities();
    runtime.freezer_ready = capabilities.iter().any(|capability| {
        capability.name == crate::domain::capability::CapabilityName::CgroupV2Freezer
            && capability.status == crate::domain::capability::CapabilityStatus::Available
    });
    state.capability_report_json = capability_report_json(&capabilities);
    state.compatibility_report_json = compatibility_report_json(&runtime, &capabilities);
    match xposed_bridge::query_hook_health() {
        Ok(payload) => {
            let status = classify_hook_health_payload(&payload);
            state.hook_health = status.health_label().to_owned();
            state.xp_log = payload;
        }
        Err(error) => {
            let status = classify_bridge_error(&error);
            state.hook_health = status.health_label().to_owned();
            state.xp_log = format!("hook bridge {}", status.health_label());
        }
    }
    runtime.hook_ready = state.hook_health == "active";
    update_diagnostics(&mut state, &runtime, &capabilities, policy_ready);
    state
}

pub fn sync_loaded_config_to_hook(
    state: &mut ReadOnlyState,
    set_app_config: impl FnOnce(&[u8]) -> Result<bool, DaemonError>,
) -> Result<bool, DaemonError> {
    let app_config_payload = encode_app_config(&state.app_config);
    let xposed_payload = encode_xposed_config_payload(&state.settings, &app_config_payload)?;
    let synced = set_app_config(&xposed_payload)?;
    state.hook_config_synced = synced;
    if synced {
        state.manager_log.push_once(LogRecord::diagnostic(
            LogLevel::Debug,
            current_timestamp_ms(),
            format!(
                "hook config synced: managed_apps={} settings={}",
                state.app_config.len(),
                state.settings.len()
            ),
        ));
    } else {
        state.manager_log.push_once(LogRecord::fault(
            LogLevel::Warn,
            current_timestamp_ms(),
            "hook config sync rejected",
        ));
    }
    Ok(synced)
}

fn import_startup_log(state: &mut ReadOnlyState, log: &str) {
    let timestamp_ms = current_timestamp_ms();
    for line in log.lines().filter(|line| !line.trim().is_empty()) {
        if let Some(record) = LogRecord::verified_legacy_line(line) {
            state.manager_log.push(record);
        } else {
            state
                .manager_log
                .push(LogRecord::diagnostic(LogLevel::Debug, timestamp_ms, line));
        }
    }
}

fn sanitize_startup_log(log: &str) -> String {
    let mut sanitized = String::new();
    let mut skipping_rom_mismatch = false;
    for line in log.lines() {
        if line.contains("WARNING ROM fingerprint mismatch") {
            skipping_rom_mismatch = true;
            continue;
        }
        if skipping_rom_mismatch {
            if line.starts_with(char::is_whitespace)
                || line.starts_with("baseline=")
                || line.starts_with("device=")
            {
                continue;
            }
            skipping_rom_mismatch = false;
        }
        sanitized.push_str(line);
        sanitized.push('\n');
    }
    sanitized
}

fn append_daemon_status_log(state: &mut ReadOnlyState) {
    state.manager_log.push_once(LogRecord::diagnostic(
        LogLevel::Debug,
        current_timestamp_ms(),
        format!(
            "daemon active: apps={} settings={} android={} kernel={}",
            state.app_config.len(),
            state.settings.len(),
            state.android_version,
            state.kernel_version
        ),
    ));
}

fn current_timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn apply_module_prop(state: &mut ReadOnlyState, path: &str) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "id" => state.module_id = value.to_owned(),
            "name" => state.module_name = value.to_owned(),
            "version" => state.version = value.to_owned(),
            "versionCode" => {
                if let Ok(version_code) = value.parse() {
                    state.version_code = version_code;
                }
            }
            "author" => state.author = value.to_owned(),
            _ => {}
        }
    }
}

fn read_first_existing_text(paths: &[String]) -> Option<String> {
    paths.iter().find_map(|path| fs::read_to_string(path).ok())
}

fn load_package_records() -> Vec<PackageRecord> {
    for (program, args) in [
        ("cmd", ["package", "list", "packages", "-U"]),
        ("pm", ["list", "packages", "-U", ""]),
    ] {
        let args = args
            .iter()
            .copied()
            .filter(|arg| !arg.is_empty())
            .collect::<Vec<_>>();
        if let Ok(output) = run_command(program, &args) {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                let records = parse_cmd_package_list(&text);
                if !records.is_empty() {
                    return records;
                }
            }
        }
    }
    Vec::new()
}

fn load_manager_app_config_records(
    app_config: Option<&str>,
    package_records: &[PackageRecord],
) -> Vec<ManagerAppConfigRecord> {
    let package_uids = package_records
        .iter()
        .map(|record| (record.package_name.as_str(), record.uid))
        .collect::<BTreeMap<_, _>>();
    app_config
        .into_iter()
        .flat_map(str::lines)
        .filter_map(parse_legacy_policy_line)
        .filter_map(|record| {
            // A package name may legitimately contain "uid" followed by digits. Resolve exact
            // inventory names first, then only accept the canonical numeric legacy encodings.
            let uid = package_uids
                .get(record.package_or_uid.as_str())
                .copied()
                .or_else(|| parse_legacy_uid_token(&record.package_or_uid))?;
            Some(ManagerAppConfigRecord {
                uid,
                mode: record.mode,
                permissive: record.permissive,
            })
        })
        .collect()
}

fn parse_legacy_uid_token(token: &str) -> Option<u32> {
    match parse_legacy_policy_target(token) {
        Some(LegacyPolicyTarget::Uid(uid)) => Some(uid),
        Some(LegacyPolicyTarget::PackageName(_)) | None => None,
    }
}

fn detect_android_version() -> String {
    command_output("getprop", &["ro.build.version.release"])
        .filter(|version| !version.is_empty())
        .unwrap_or_else(|| std::env::consts::OS.to_owned())
}

fn detect_runtime_environment() -> RuntimeEnvironment {
    RuntimeEnvironment::new(
        command_output("getprop", &["ro.product.model"]).unwrap_or_else(|| "unknown".to_owned()),
        detect_android_version(),
        command_output("getprop", &["ro.build.version.sdk"])
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        command_output("getprop", &["ro.build.fingerprint"]).unwrap_or_default(),
        detect_kernel_version(),
        unsafe { libc::geteuid() == 0 },
        false,
        false,
    )
}

fn update_diagnostics(
    state: &mut ReadOnlyState,
    runtime: &RuntimeEnvironment,
    capabilities: &[ControlCapability],
    policy_ready: bool,
) {
    let health = ModuleHealth::evaluate(
        true,
        true,
        runtime.hook_ready,
        runtime.root_ready,
        runtime.freezer_ready,
        policy_ready,
    );
    state.health_report_json = health_report_json(&health);
    state.self_check_json = self_check_json_for_runtime(&health, capabilities, runtime);
    state.compatibility_report_json = compatibility_report_json(runtime, capabilities);
    state.control_allowed = runtime.allows_control(capabilities);
}

pub fn refresh_runtime_diagnostics(state: &mut ReadOnlyState) {
    let capabilities = discover_runtime_capabilities();
    let mut runtime = detect_runtime_environment();
    runtime.verified_targets = state
        .verified_targets
        .iter()
        .map(|(model, sdk)| crate::app::compatibility::VerifiedTarget::new(model, *sdk))
        .collect();
    runtime.hook_ready = state.hook_health == "active";
    runtime.freezer_ready = capabilities.iter().any(|capability| {
        capability.name == crate::domain::capability::CapabilityName::CgroupV2Freezer
            && capability.status == crate::domain::capability::CapabilityStatus::Available
    });
    let policy_ready = state
        .settings_path
        .as_deref()
        .is_some_and(|path| std::path::Path::new(path).exists())
        || state
            .app_config_path
            .as_deref()
            .is_some_and(|path| std::path::Path::new(path).exists());
    state.capability_report_json = capability_report_json(&capabilities);
    update_diagnostics(state, &runtime, &capabilities, policy_ready);
}

fn detect_kernel_version() -> String {
    command_output("uname", &["-r"])
        .filter(|version| !version.is_empty())
        .or_else(|| {
            fs::read_to_string("/proc/version")
                .ok()
                .and_then(|text| text.split_whitespace().nth(2).map(str::to_owned))
        })
        .unwrap_or_else(|| std::env::consts::ARCH.to_owned())
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = run_command(program, args).ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn detect_cpu_cluster_count() -> u32 {
    let Ok(entries) = fs::read_dir("/sys/devices/system/cpu") else {
        return 1;
    };
    let core_count = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            name.strip_prefix("cpu").is_some_and(|suffix| {
                !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit())
            })
        })
        .count() as u32;
    core_count.max(1)
}

fn detect_ext_memory_mib() -> u32 {
    fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                let mut parts = line.split_whitespace();
                (parts.next()? == "SwapTotal:")
                    .then(|| parts.next()?.parse::<u32>().ok().map(|kb| kb / 1024))
                    .flatten()
            })
        })
        .unwrap_or(0)
}

pub fn handle_manager_read_only(
    command: ManagerCommand,
    state: &ReadOnlyState,
) -> Result<Vec<u8>, DaemonError> {
    handle_read_only_command(command, state)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticState {
    pub health: ModuleHealth,
    pub capabilities: Vec<ControlCapability>,
    pub operation_log: OperationLog,
}

pub fn read_only_state_with_diagnostics(diagnostics: &DiagnosticState) -> ReadOnlyState {
    let mut state = ReadOnlyState {
        health_report_json: health_report_json(&diagnostics.health),
        capability_report_json: capability_report_json(&diagnostics.capabilities),
        compatibility_report_json: compatibility_report_json(
            &RuntimeEnvironment::new("unknown", "unknown", 0, "", "unknown", false, false, false),
            &diagnostics.capabilities,
        ),
        operation_log_json: operation_log_json(&diagnostics.operation_log),
        ..ReadOnlyState::default()
    };
    for operation in diagnostics.operation_log.records() {
        state
            .manager_log
            .push(LogRecord::operation(operation.clone()));
    }
    state.self_check_json = self_check_json(&diagnostics.health, &diagnostics.capabilities);
    state
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeControlState {
    pub operation_log: OperationLog,
    frozen_apps: std::collections::BTreeSet<(String, u32)>,
    frozen_since_ms: BTreeMap<RuntimeIdentity, u128>,
    frozen_ownership: BTreeMap<RuntimeIdentity, FrozenOwnership>,
    pending_freezes: BTreeMap<(String, u32), u128>,
    network_restriction_requests: BTreeSet<u32>,
    next_operation_id: u64,
    status_since_ms: BTreeMap<u32, (i32, u128)>,
    blocked_process_signatures: BTreeMap<RuntimeIdentity, ProcessSignature>,
    download_deferral: DownloadDeferral,
    audit_started_at_ms: Option<u128>,
    next_refreeze_audit_at_ms: Option<u128>,
}

type RuntimeIdentity = (String, u32);
type ProcessSignature = Vec<(i32, String, Option<u64>)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrozenOwnership {
    SignalOnly,
    CgroupBinder,
    ResidualUnknown,
}

impl FrozenOwnership {
    fn unfreeze_backend(self, processes: &[RuntimeProcess]) -> &'static str {
        match self {
            Self::SignalOnly => "signal.cont",
            Self::CgroupBinder | Self::ResidualUnknown => backend_name(processes),
        }
    }
}

/// Reconcile live process control state from kernel-visible evidence before a control pass.
///
/// Proc discovery starts with `Running` because it cannot read every per-process state while
/// walking `/proc`. A cgroup read failure or a raced-out proc entry must not turn that default
/// into evidence that the process was thawed, so uncertain observations remain `Unknown`.
pub fn reconcile_live_process_control_states(
    processes: &mut [RuntimeProcess],
    mut read_cgroup_state: impl FnMut(&str) -> Result<cgroup::FreezeState, DaemonError>,
    mut read_proc_state: impl FnMut(i32) -> Result<char, DaemonError>,
) {
    for process in processes {
        let cgroup_state = match process.cgroup_freeze_path.as_deref() {
            Some(path) => read_cgroup_state(path).ok(),
            // No cgroup freezer is attached to this process, so only the signal state can
            // establish that it is frozen.
            None => Some(cgroup::FreezeState::Thawed),
        };

        if cgroup_state == Some(cgroup::FreezeState::Frozen) {
            process.control_state = ControlState::Frozen;
            continue;
        }

        let proc_state = read_proc_state(process.pid).ok();
        process.control_state = if matches!(proc_state, Some('T' | 't')) {
            ControlState::Frozen
        } else if cgroup_state == Some(cgroup::FreezeState::Thawed) && proc_state.is_some() {
            ControlState::Running
        } else {
            ControlState::Unknown
        };
    }
}

impl Default for RuntimeControlState {
    fn default() -> Self {
        Self {
            operation_log: OperationLog::new(128),
            frozen_apps: std::collections::BTreeSet::new(),
            frozen_since_ms: BTreeMap::new(),
            frozen_ownership: BTreeMap::new(),
            pending_freezes: BTreeMap::new(),
            network_restriction_requests: BTreeSet::new(),
            next_operation_id: 1,
            status_since_ms: BTreeMap::new(),
            blocked_process_signatures: BTreeMap::new(),
            download_deferral: DownloadDeferral::default(),
            audit_started_at_ms: None,
            next_refreeze_audit_at_ms: None,
        }
    }
}

impl RuntimeControlState {
    fn track_frozen(
        &mut self,
        identity: RuntimeIdentity,
        timestamp_ms: u128,
        ownership: FrozenOwnership,
    ) {
        let ownership = if self.frozen_ownership.get(&identity)
            == Some(&FrozenOwnership::ResidualUnknown)
            || (self.frozen_apps.contains(&identity)
                && !self.frozen_ownership.contains_key(&identity))
        {
            FrozenOwnership::ResidualUnknown
        } else {
            ownership
        };
        self.frozen_apps.insert(identity.clone());
        self.frozen_since_ms
            .entry(identity.clone())
            .or_insert(timestamp_ms);
        self.frozen_ownership.insert(identity, ownership);
    }

    fn frozen_ownership(&self, identity: &RuntimeIdentity) -> FrozenOwnership {
        self.frozen_ownership
            .get(identity)
            .copied()
            .unwrap_or(FrozenOwnership::ResidualUnknown)
    }

    fn has_unresolved_freeze_ownership(&self, identity: &RuntimeIdentity) -> bool {
        matches!(
            self.frozen_ownership.get(identity),
            Some(FrozenOwnership::CgroupBinder | FrozenOwnership::ResidualUnknown)
        ) || (self.frozen_apps.contains(identity) && !self.frozen_ownership.contains_key(identity))
    }

    fn clear_frozen(&mut self, identity: &RuntimeIdentity) {
        self.frozen_apps.remove(identity);
        self.frozen_since_ms.remove(identity);
        self.frozen_ownership.remove(identity);
    }

    fn mark_abnormal_thaw_for_refreeze(&mut self, identity: &RuntimeIdentity) {
        let ownership = match self.frozen_ownership(identity) {
            FrozenOwnership::SignalOnly => FrozenOwnership::SignalOnly,
            FrozenOwnership::CgroupBinder | FrozenOwnership::ResidualUnknown => {
                FrozenOwnership::ResidualUnknown
            }
        };
        self.frozen_apps.remove(identity);
        self.frozen_since_ms.remove(identity);
        self.frozen_ownership.insert(identity.clone(), ownership);
    }

    fn clear_detached_ownership(&mut self, identity: &RuntimeIdentity) {
        if !self.frozen_apps.contains(identity) {
            self.frozen_ownership.remove(identity);
        }
    }

    pub fn pending_freeze_uids(&self) -> Vec<u32> {
        self.pending_freezes
            .keys()
            .map(|(_, uid)| *uid)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn take_network_restriction_uids(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.network_restriction_requests)
            .into_iter()
            .collect()
    }

    pub fn requeue_network_restriction_uid(&mut self, uid: u32) {
        self.network_restriction_requests.insert(uid);
    }

    fn clear_transient_control_state_for_uid(&mut self, uid: u32) {
        self.pending_freezes
            .retain(|(_, pending_uid), _| *pending_uid != uid);
        self.network_restriction_requests.remove(&uid);
        self.blocked_process_signatures
            .retain(|(_, blocked_uid), _| *blocked_uid != uid);
        self.download_deferral.clear_uid(uid);
        self.status_since_ms.remove(&uid);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn thaw_all_frozen(
        &mut self,
        mut discover_processes: impl FnMut(&str, u32) -> Result<Vec<RuntimeProcess>, DaemonError>,
        mut unfreeze_process: impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
        mut validate_process: impl FnMut(&RuntimeProcess) -> Result<bool, DaemonError>,
        timestamp_ms: u128,
    ) -> Result<(), DaemonError> {
        let identities = self.frozen_apps.iter().cloned().collect::<Vec<_>>();
        let result = thaw_frozen_identities(
            self,
            &identities,
            &mut discover_processes,
            &mut unfreeze_process,
            &mut validate_process,
            timestamp_ms,
            "control disabled",
        );
        let detached_ownership_identities = self
            .frozen_ownership
            .keys()
            .filter(|identity| !self.frozen_apps.contains(*identity))
            .cloned()
            .collect::<Vec<_>>();
        for identity in detached_ownership_identities {
            self.clear_detached_ownership(&identity);
        }
        self.pending_freezes.clear();
        self.network_restriction_requests.clear();
        self.blocked_process_signatures.clear();
        self.download_deferral = DownloadDeferral::default();
        result
    }

    pub fn freeze_status_records(
        &mut self,
        app_config: &[ManagerAppConfigRecord],
        processes_by_uid: &BTreeMap<u32, Vec<RuntimeProcess>>,
        foreground_uids: &[u32],
        timestamp_ms: u128,
    ) -> Vec<ManagerFreezeStatusRecord> {
        const STATE_RUNNING_BACKGROUND: i32 = 0;
        const STATE_FOREGROUND: i32 = 1;
        const STATE_PENDING: i32 = 2;
        const STATE_FROZEN: i32 = 3;

        let mut records = Vec::new();
        let mut visible_uids = std::collections::BTreeSet::new();
        let foreground_uids = foreground_uids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let pending_by_uid =
            self.pending_freezes
                .iter()
                .fold(BTreeMap::new(), |mut pending, ((_, uid), due)| {
                    pending
                        .entry(*uid)
                        .and_modify(|current: &mut u128| *current = (*current).min(*due))
                        .or_insert(*due);
                    pending
                });
        let frozen_uids = self
            .frozen_apps
            .iter()
            .map(|(_, uid)| *uid)
            .collect::<std::collections::BTreeSet<_>>();
        for config in app_config
            .iter()
            .filter(|record| is_control_policy_mode(record.mode))
        {
            let processes = processes_by_uid
                .get(&config.uid)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let is_foreground = foreground_uids.contains(&config.uid);
            let pending_due = pending_by_uid.get(&config.uid).copied();
            let is_frozen = frozen_uids.contains(&config.uid);
            if processes.is_empty() && !is_foreground && pending_due.is_none() && !is_frozen {
                continue;
            }

            let state = if is_foreground {
                STATE_FOREGROUND
            } else if pending_due.is_some() {
                STATE_PENDING
            } else if is_frozen {
                STATE_FROZEN
            } else {
                STATE_RUNNING_BACKGROUND
            };
            visible_uids.insert(config.uid);
            let seconds = if let Some(due_at_ms) = pending_due {
                due_at_ms
                    .saturating_sub(timestamp_ms)
                    .div_ceil(1_000)
                    .min(i32::MAX as u128) as i32
            } else {
                let since = match self.status_since_ms.get(&config.uid).copied() {
                    Some((previous_state, since)) if previous_state == state => since,
                    _ => timestamp_ms,
                };
                self.status_since_ms.insert(config.uid, (state, since));
                timestamp_ms
                    .saturating_sub(since)
                    .checked_div(1_000)
                    .unwrap_or(0)
                    .min(i32::MAX as u128) as i32
            };
            records.push(ManagerFreezeStatusRecord {
                uid: config.uid,
                foreground: is_foreground,
                state,
                seconds,
                process_count: processes.len().min(i32::MAX as usize) as i32,
            });
        }
        self.status_since_ms
            .retain(|uid, _| visible_uids.contains(uid));
        records.sort_by_key(|record| {
            let rank = match record.state {
                STATE_FOREGROUND => 0,
                STATE_PENDING => 1,
                STATE_RUNNING_BACKGROUND => 2,
                STATE_FROZEN => 3,
                _ => 4,
            };
            (rank, record.uid)
        });
        records
    }
}

#[derive(Debug, Default)]
struct UnfreezeOutcome {
    applied: usize,
    failures: Vec<String>,
}

fn unfreeze_identity_processes(
    expected_package: &str,
    processes: &[RuntimeProcess],
    ownership: FrozenOwnership,
    validate_process: &mut impl FnMut(&RuntimeProcess) -> Result<bool, DaemonError>,
    unfreeze_process: &mut impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
) -> UnfreezeOutcome {
    if processes
        .iter()
        .any(|process| process.package_name != expected_package)
    {
        return UnfreezeOutcome {
            applied: 0,
            failures: vec!["shared uid contains multiple package identities".to_owned()],
        };
    }

    let mut outcome = UnfreezeOutcome::default();
    for process in processes {
        let signal_process =
            (ownership == FrozenOwnership::SignalOnly).then(|| signal_control_process(process));
        let control_process = signal_process.as_ref().unwrap_or(process);
        match validate_process(control_process) {
            Ok(true) => {}
            Ok(false) => {
                outcome.failures.push(format!(
                    "identity validation failed before unfreeze for pid {}",
                    control_process.pid
                ));
                continue;
            }
            Err(error) => {
                outcome.failures.push(format!(
                    "identity validation failed before unfreeze for pid {}: {error}",
                    control_process.pid
                ));
                continue;
            }
        }
        match unfreeze_process(control_process) {
            Ok(()) => outcome.applied += 1,
            Err(error) => outcome.failures.push(format!(
                "unfreeze failed for pid {}: {error}",
                control_process.pid
            )),
        }
    }
    outcome
}

#[allow(clippy::too_many_arguments)]
fn thaw_frozen_identities(
    state: &mut RuntimeControlState,
    identities: &[RuntimeIdentity],
    discover_processes: &mut impl FnMut(&str, u32) -> Result<Vec<RuntimeProcess>, DaemonError>,
    unfreeze_process: &mut impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
    validate_process: &mut impl FnMut(&RuntimeProcess) -> Result<bool, DaemonError>,
    timestamp_ms: u128,
    reason: &str,
) -> Result<(), DaemonError> {
    let mut failures = Vec::new();
    for identity @ (package_name, uid) in identities {
        let fallback_package_name = format!("uid{uid}");
        let processes = match discover_processes(&fallback_package_name, *uid) {
            Ok(processes) => processes,
            Err(error) => {
                let message = format!("{reason}; process discovery failed: {error}");
                let mut operation = ControlOperation {
                    operation_id: 0,
                    timestamp_ms: 0,
                    package_name: package_name.clone(),
                    uid: *uid,
                    pid_list: Vec::new(),
                    action: ControlAction::Unfreeze,
                    backend: "recovery".to_owned(),
                    reason: message.clone(),
                    result: OperationResult::Failed,
                    details: "process_count=0".to_owned(),
                };
                stamp_operation(&mut operation, state, timestamp_ms);
                state.operation_log.push(operation);
                failures.push(message);
                continue;
            }
        };
        if processes.is_empty() {
            state.clear_frozen(identity);
            continue;
        }

        let ownership = state.frozen_ownership(identity);
        let outcome = unfreeze_identity_processes(
            package_name,
            &processes,
            ownership,
            validate_process,
            unfreeze_process,
        );
        let succeeded = outcome.failures.is_empty();
        if succeeded {
            state.clear_frozen(identity);
        }
        let result = if succeeded {
            OperationResult::Success
        } else if outcome.applied == 0 {
            OperationResult::Failed
        } else {
            OperationResult::Partial
        };
        let mut operation = ControlOperation {
            operation_id: 0,
            timestamp_ms: 0,
            package_name: package_name.clone(),
            uid: *uid,
            pid_list: processes.iter().map(|process| process.pid).collect(),
            action: ControlAction::Unfreeze,
            backend: ownership.unfreeze_backend(&processes).to_owned(),
            reason: if succeeded {
                reason.to_owned()
            } else {
                format!("{reason}; {}", outcome.failures.join("; "))
            },
            result,
            details: operation_details(&processes),
        };
        stamp_operation(&mut operation, state, timestamp_ms);
        state.operation_log.push(operation);
        if !succeeded {
            failures.extend(outcome.failures);
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(DaemonError::system(failures.join("; ")))
    }
}

fn manager_wakeup_interval_ms(settings: &[u8]) -> Option<u128> {
    match settings.get(3).copied().unwrap_or(0) {
        1 => Some(5 * 60_000),
        2 => Some(15 * 60_000),
        3 => Some(30 * 60_000),
        4 => Some(60 * 60_000),
        5 => Some(120 * 60_000),
        _ => None,
    }
}

fn manager_wakeup_duration_ms(settings: &[u8]) -> u128 {
    u128::from(settings.get(2).copied().unwrap_or(0)) * 1_000
}

fn periodic_wakeup_due(
    state: &RuntimeControlState,
    identity: &RuntimeIdentity,
    settings: &[u8],
    timestamp_ms: u128,
) -> bool {
    let Some(interval_ms) = manager_wakeup_interval_ms(settings) else {
        return false;
    };
    let Some(frozen_since_ms) = state.frozen_since_ms.get(identity).copied() else {
        return false;
    };
    timestamp_ms >= frozen_since_ms.saturating_add(interval_ms)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestedControlBackend {
    Terminate,
    Signal,
    Freezer,
}

impl RequestedControlBackend {
    fn operation_backend(self, processes: &[RuntimeProcess]) -> &'static str {
        match self {
            Self::Terminate => "signal.kill",
            Self::Signal => "signal.stop",
            Self::Freezer => backend_name(processes),
        }
    }
}

fn requested_control_backend(
    record: &ManagerAppConfigRecord,
    settings: &[u8],
) -> RequestedControlBackend {
    match record.mode {
        10 => RequestedControlBackend::Terminate,
        20 | 21 => RequestedControlBackend::Signal,
        30 | 31 if settings.get(5).copied() == Some(2) => RequestedControlBackend::Signal,
        _ => RequestedControlBackend::Freezer,
    }
}

fn mode_has_network_restriction(mode: i32) -> bool {
    matches!(mode, 21 | 31)
}

fn signal_control_process(process: &RuntimeProcess) -> RuntimeProcess {
    let mut signal_process = process.clone();
    signal_process.cgroup_freeze_path = None;
    signal_process
}

pub fn run_control_pass(
    state: &mut RuntimeControlState,
    app_config: &[ManagerAppConfigRecord],
    mut discover_processes: impl FnMut(&str, u32) -> Result<Vec<RuntimeProcess>, DaemonError>,
    mut freeze_process: impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
    mut unfreeze_process: impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
    foreground_uids: &[u32],
    timestamp_ms: u128,
) -> Result<(), DaemonError> {
    run_control_pass_with_settings(
        state,
        app_config,
        &[],
        &mut discover_processes,
        &mut freeze_process,
        &mut unfreeze_process,
        foreground_uids,
        timestamp_ms,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn run_control_pass_with_settings(
    state: &mut RuntimeControlState,
    app_config: &[ManagerAppConfigRecord],
    settings: &[u8],
    mut discover_processes: impl FnMut(&str, u32) -> Result<Vec<RuntimeProcess>, DaemonError>,
    mut freeze_process: impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
    mut unfreeze_process: impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
    foreground_uids: &[u32],
    timestamp_ms: u128,
) -> Result<(), DaemonError> {
    run_control_pass_with_validation(
        state,
        app_config,
        settings,
        &mut discover_processes,
        &mut freeze_process,
        &mut unfreeze_process,
        |_| Ok(true),
        foreground_uids,
        timestamp_ms,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn run_control_pass_with_validation(
    state: &mut RuntimeControlState,
    app_config: &[ManagerAppConfigRecord],
    settings: &[u8],
    mut discover_processes: impl FnMut(&str, u32) -> Result<Vec<RuntimeProcess>, DaemonError>,
    mut freeze_process: impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
    mut unfreeze_process: impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
    mut validate_process: impl FnMut(&RuntimeProcess) -> Result<bool, DaemonError>,
    foreground_uids: &[u32],
    timestamp_ms: u128,
) -> Result<(), DaemonError> {
    run_control_pass_with_sampling(
        state,
        app_config,
        settings,
        &mut discover_processes,
        &mut freeze_process,
        &mut unfreeze_process,
        &mut validate_process,
        |uid, _| sample_uid_rx_bytes(uid),
        foreground_uids,
        timestamp_ms,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn run_control_pass_with_sampling(
    state: &mut RuntimeControlState,
    app_config: &[ManagerAppConfigRecord],
    settings: &[u8],
    mut discover_processes: impl FnMut(&str, u32) -> Result<Vec<RuntimeProcess>, DaemonError>,
    mut freeze_process: impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
    mut unfreeze_process: impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
    mut validate_process: impl FnMut(&RuntimeProcess) -> Result<bool, DaemonError>,
    mut sample_rx_bytes: impl FnMut(u32, &str) -> Result<Option<u64>, DaemonError>,
    foreground_uids: &[u32],
    timestamp_ms: u128,
) -> Result<(), DaemonError> {
    let mut terminate_process = terminate_runtime_process;
    run_control_pass_with_sampling_and_terminate(
        state,
        app_config,
        settings,
        &mut discover_processes,
        &mut freeze_process,
        &mut unfreeze_process,
        &mut validate_process,
        &mut sample_rx_bytes,
        &mut terminate_process,
        foreground_uids,
        timestamp_ms,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_control_pass_with_sampling_and_terminate(
    state: &mut RuntimeControlState,
    app_config: &[ManagerAppConfigRecord],
    settings: &[u8],
    mut discover_processes: impl FnMut(&str, u32) -> Result<Vec<RuntimeProcess>, DaemonError>,
    mut freeze_process: impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
    mut unfreeze_process: impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
    mut validate_process: impl FnMut(&RuntimeProcess) -> Result<bool, DaemonError>,
    mut sample_rx_bytes: impl FnMut(u32, &str) -> Result<Option<u64>, DaemonError>,
    mut terminate_process: impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
    foreground_uids: &[u32],
    timestamp_ms: u128,
) -> Result<(), DaemonError> {
    let controlled_uids = app_config
        .iter()
        .filter(|record| is_control_policy_mode(record.mode))
        .map(|record| record.uid)
        .collect::<BTreeSet<_>>();
    let stale_frozen_identities = state
        .frozen_apps
        .iter()
        .filter(|(_, uid)| !controlled_uids.contains(uid))
        .cloned()
        .collect::<Vec<_>>();
    let stale_uids = state
        .pending_freezes
        .keys()
        .map(|(_, uid)| *uid)
        .chain(stale_frozen_identities.iter().map(|(_, uid)| *uid))
        .chain(state.frozen_ownership.keys().map(|(_, uid)| *uid))
        .filter(|uid| !controlled_uids.contains(uid))
        .collect::<BTreeSet<_>>();
    state
        .pending_freezes
        .retain(|(_, uid), _| controlled_uids.contains(uid));
    state
        .blocked_process_signatures
        .retain(|(_, uid), _| controlled_uids.contains(uid));
    state
        .network_restriction_requests
        .retain(|uid| controlled_uids.contains(uid));
    state
        .status_since_ms
        .retain(|uid, _| controlled_uids.contains(uid));
    for uid in stale_uids {
        state.download_deferral.clear_uid(uid);
    }
    // A record that became Free/whitelisted or was removed no longer participates in
    // the main loop. Thaw it before processing the remaining controlled records.
    let _ = thaw_frozen_identities(
        state,
        &stale_frozen_identities,
        &mut discover_processes,
        &mut unfreeze_process,
        &mut validate_process,
        timestamp_ms,
        "control policy removed",
    );
    let stale_detached_ownership_identities = state
        .frozen_ownership
        .keys()
        .filter(|identity| {
            !state.frozen_apps.contains(*identity) && !controlled_uids.contains(&identity.1)
        })
        .cloned()
        .collect::<Vec<_>>();
    for identity in stale_detached_ownership_identities {
        state.clear_detached_ownership(&identity);
    }

    let audit_due = refreeze_audit_due(state, settings, timestamp_ms);
    for record in app_config {
        if !is_control_policy_mode(record.mode) {
            continue;
        }
        let fallback_package_name = format!("uid{}", record.uid);
        let processes = discover_processes(&fallback_package_name, record.uid)?;
        if processes.is_empty() {
            state.clear_transient_control_state_for_uid(record.uid);
            let tracked_identities = state
                .frozen_apps
                .iter()
                .chain(state.frozen_ownership.keys())
                .filter(|(_, uid)| *uid == record.uid)
                .cloned()
                .collect::<BTreeSet<_>>();
            for identity in tracked_identities {
                state.clear_frozen(&identity);
            }
            continue;
        }
        let package_name = processes
            .iter()
            .map(|process| process.package_name.as_str())
            .min()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| fallback_package_name.clone());
        let app = managed_app_from_record(&package_name, record.uid);
        let policy = policy_from_record(record);
        let identity = (package_name.clone(), record.uid);
        if !policy.is_control_allowed_for(&app) {
            state.clear_transient_control_state_for_uid(record.uid);
            let frozen_identities = state
                .frozen_apps
                .iter()
                .filter(|(_, uid)| *uid == record.uid)
                .cloned()
                .collect::<Vec<_>>();
            let _ = thaw_frozen_identities(
                state,
                &frozen_identities,
                &mut discover_processes,
                &mut unfreeze_process,
                &mut validate_process,
                timestamp_ms,
                "policy or protected package blocks control",
            );
            let detached_ownership_identities = state
                .frozen_ownership
                .keys()
                .filter(|identity| {
                    identity.1 == record.uid && !state.frozen_apps.contains(*identity)
                })
                .cloned()
                .collect::<Vec<_>>();
            for identity in detached_ownership_identities {
                state.clear_detached_ownership(&identity);
            }
            continue;
        }
        let mut process_signature = processes
            .iter()
            .map(|process| {
                (
                    process.pid,
                    process.package_name.clone(),
                    process.start_time_ticks,
                )
            })
            .collect::<Vec<_>>();
        process_signature.sort();
        match state.blocked_process_signatures.get(&identity) {
            Some(blocked_signature) if blocked_signature == &process_signature => continue,
            Some(_) => {
                state.blocked_process_signatures.remove(&identity);
            }
            None => {}
        }

        if foreground_uids.contains(&record.uid) {
            state.pending_freezes.remove(&identity);
            if state.frozen_apps.contains(&identity) {
                let ownership = state.frozen_ownership(&identity);
                let outcome = unfreeze_identity_processes(
                    &package_name,
                    &processes,
                    ownership,
                    &mut validate_process,
                    &mut unfreeze_process,
                );
                let succeeded = outcome.failures.is_empty();
                if succeeded {
                    state.clear_frozen(&identity);
                }
                let mut operation = ControlOperation {
                    operation_id: 0,
                    timestamp_ms: 0,
                    package_name: package_name.clone(),
                    uid: record.uid,
                    pid_list: processes.iter().map(|process| process.pid).collect(),
                    action: ControlAction::Unfreeze,
                    backend: ownership.unfreeze_backend(&processes).to_owned(),
                    reason: if succeeded {
                        "foreground uid active".to_owned()
                    } else {
                        outcome.failures.join("; ")
                    },
                    result: if succeeded {
                        OperationResult::Success
                    } else if outcome.applied == 0 {
                        OperationResult::Failed
                    } else {
                        OperationResult::Partial
                    },
                    details: operation_details(&processes),
                };
                stamp_operation(&mut operation, state, timestamp_ms);
                state.operation_log.push(operation);
            }
            continue;
        }

        let mut abnormal_thaw = false;
        if state.frozen_apps.contains(&identity) {
            if periodic_wakeup_due(state, &identity, settings, timestamp_ms) {
                let ownership = state.frozen_ownership(&identity);
                let outcome = unfreeze_identity_processes(
                    &package_name,
                    &processes,
                    ownership,
                    &mut validate_process,
                    &mut unfreeze_process,
                );
                let succeeded = outcome.failures.is_empty();
                if succeeded {
                    state.clear_frozen(&identity);
                    state.pending_freezes.insert(
                        identity.clone(),
                        timestamp_ms.saturating_add(manager_wakeup_duration_ms(settings)),
                    );
                }
                let mut operation = ControlOperation {
                    operation_id: 0,
                    timestamp_ms: 0,
                    package_name: package_name.clone(),
                    uid: record.uid,
                    pid_list: processes.iter().map(|process| process.pid).collect(),
                    action: ControlAction::Unfreeze,
                    backend: ownership.unfreeze_backend(&processes).to_owned(),
                    reason: if succeeded {
                        format!(
                            "periodic wakeup after {}ms; refreeze in {}ms",
                            manager_wakeup_interval_ms(settings).unwrap_or_default(),
                            manager_wakeup_duration_ms(settings)
                        )
                    } else {
                        format!("periodic wakeup failed; {}", outcome.failures.join("; "))
                    },
                    result: if succeeded {
                        OperationResult::Success
                    } else if outcome.applied == 0 {
                        OperationResult::Failed
                    } else {
                        OperationResult::Partial
                    },
                    details: operation_details(&processes),
                };
                stamp_operation(&mut operation, state, timestamp_ms);
                state.operation_log.push(operation);
                continue;
            }
            if !audit_due
                || !processes
                    .iter()
                    .any(|process| process.control_state == ControlState::Running)
            {
                continue;
            }
            state.mark_abnormal_thaw_for_refreeze(&identity);
            abnormal_thaw = true;
        }

        let delay_ms = manager_policy_delay_ms(record, settings);
        if !abnormal_thaw && delay_ms > 0 {
            match state.pending_freezes.get(&identity).copied() {
                Some(due_at_ms) if timestamp_ms < due_at_ms => continue,
                Some(_) => {}
                None => {
                    state
                        .pending_freezes
                        .insert(identity.clone(), timestamp_ms + u128::from(delay_ms));
                    let mut operation = ControlOperation {
                        operation_id: 0,
                        timestamp_ms: 0,
                        package_name,
                        uid: record.uid,
                        pid_list: processes.iter().map(|process| process.pid).collect(),
                        action: ControlAction::Postpone,
                        backend: backend_name(&processes).to_owned(),
                        reason: format!("pending freeze delay {delay_ms}ms"),
                        result: OperationResult::Postponed,
                        details: operation_details(&processes),
                    };
                    stamp_operation(&mut operation, state, timestamp_ms);
                    state.operation_log.push(operation);
                    if !state.frozen_apps.contains(&identity) {
                        state.frozen_since_ms.remove(&identity);
                    }
                    continue;
                }
            }
        }

        if !crate::app::download_deferral::is_candidate_package(&package_name) {
            state.download_deferral.evaluate(
                record.uid,
                &package_name,
                UidRxSample::Missing,
                timestamp_ms,
            );
        } else {
            let sample = sample_rx_bytes(record.uid, &package_name);
            let sample_value = match &sample {
                Ok(Some(value)) => UidRxSample::Value(*value),
                Ok(None) => UidRxSample::Missing,
                Err(_) => UidRxSample::Failed,
            };
            if state.download_deferral.evaluate(
                record.uid,
                &package_name,
                sample_value,
                timestamp_ms,
            ) == DownloadDeferralAction::Postpone
            {
                let retry_ms = if sample.is_ok() {
                    INITIAL_SAMPLE_DELAY_MS
                } else {
                    DOWNLOAD_RETRY_DELAY_MS
                };
                let previous_due = state
                    .pending_freezes
                    .insert(identity.clone(), timestamp_ms + u128::from(retry_ms));
                if previous_due.is_none() {
                    let mut operation = ControlOperation {
                        operation_id: 0,
                        timestamp_ms: 0,
                        package_name,
                        uid: record.uid,
                        pid_list: processes.iter().map(|process| process.pid).collect(),
                        action: ControlAction::Postpone,
                        backend: backend_name(&processes).to_owned(),
                        reason: if sample.is_ok() {
                            "download activity sampling/threshold".to_owned()
                        } else {
                            "download sampling failed; fail-safe postpone".to_owned()
                        },
                        result: OperationResult::Postponed,
                        details: operation_details(&processes),
                    };
                    stamp_operation(&mut operation, state, timestamp_ms);
                    state.operation_log.push(operation);
                }
                if !state.frozen_apps.contains(&identity) {
                    state.frozen_since_ms.remove(&identity);
                }
                continue;
            }
        }

        let mut pending_processes = processes.clone();
        for process in &mut pending_processes {
            process.control_state = crate::domain::runtime::ControlState::PendingFreeze;
        }
        let backend = SystemAwareCgroupBinderBackend::new(backend_environment(&pending_processes));
        let requested_backend = requested_control_backend(record, settings);
        let decision = if !policy.is_control_allowed_for(&app) {
            FreezeDecision {
                action: DecisionAction::Skip,
                reason: "policy or protected package blocks control".to_owned(),
            }
        } else {
            match requested_backend {
                RequestedControlBackend::Terminate => FreezeDecision {
                    action: DecisionAction::Terminate,
                    reason: "SIGKILL selected by manager policy".to_owned(),
                },
                RequestedControlBackend::Signal => FreezeDecision {
                    action: DecisionAction::Signal,
                    reason: if record.mode == 20 || record.mode == 21 {
                        "SIGSTOP selected by manager policy".to_owned()
                    } else {
                        "global SIGSTOP selected for freezer policy".to_owned()
                    },
                },
                RequestedControlBackend::Freezer => {
                    backend.can_freeze(&app, &policy, &pending_processes)
                }
            }
        };
        let (action, result) = match decision.action {
            DecisionAction::Freeze => {
                let freeze_outcome = apply_freeze_transaction(
                    &pending_processes,
                    &mut validate_process,
                    &mut freeze_process,
                    &mut unfreeze_process,
                );
                if let Some(failure) = freeze_outcome.failure {
                    if failure.contains("shared uid contains multiple package identities") {
                        state
                            .blocked_process_signatures
                            .insert(identity.clone(), process_signature.clone());
                    }
                    if freeze_outcome.residual_possible {
                        state.track_frozen(
                            identity.clone(),
                            timestamp_ms,
                            FrozenOwnership::ResidualUnknown,
                        );
                    }
                    let error = DaemonError::system(failure.clone());
                    let fallback = backend.fallback_after_freeze_apply_error(&policy, &error);
                    let (action, result, fallback_backend, fallback_details) = match fallback.action
                    {
                        DecisionAction::Signal => {
                            let outcome = apply_signal_stop_freeze_transaction(
                                &pending_processes,
                                &mut validate_process,
                                &mut freeze_process,
                                &mut unfreeze_process,
                                SignalStopLedgerPromotion::Never,
                                &mut |_| Ok(()),
                            );
                            if outcome.failure.is_none() {
                                state.track_frozen(
                                    identity.clone(),
                                    timestamp_ms,
                                    FrozenOwnership::SignalOnly,
                                );
                                if mode_has_network_restriction(record.mode) {
                                    state.network_restriction_requests.insert(record.uid);
                                }
                                (
                                    ControlAction::Freeze,
                                    OperationResult::Success,
                                    "signal.stop",
                                    format!("signal_applied={}", outcome.applied),
                                )
                            } else {
                                if outcome.rollback_failures > 0 {
                                    state.track_frozen(
                                        identity.clone(),
                                        timestamp_ms,
                                        FrozenOwnership::ResidualUnknown,
                                    );
                                }
                                (
                                    ControlAction::Fallback,
                                    if outcome.rollback_failures == 0 {
                                        OperationResult::Failed
                                    } else {
                                        OperationResult::Partial
                                    },
                                    "signal.stop",
                                    format!(
                                        "signal_failure={} signal_applied={} signal_rolled_back={} signal_rollback_failures={}",
                                        outcome.failure.unwrap_or_default(),
                                        outcome.applied,
                                        outcome.rolled_back,
                                        outcome.rollback_failures
                                    ),
                                )
                            }
                        }
                        other => {
                            let (action, result) = operation_from_fallback(other);
                            (
                                action,
                                result,
                                backend_name(&pending_processes),
                                String::new(),
                            )
                        }
                    };
                    if result == OperationResult::Success
                        && action == ControlAction::Freeze
                        && mode_has_network_restriction(record.mode)
                    {
                        state.network_restriction_requests.insert(record.uid);
                    }
                    state.pending_freezes.remove(&identity);
                    let mut operation = ControlOperation {
                        operation_id: 0,
                        timestamp_ms: 0,
                        package_name,
                        uid: record.uid,
                        pid_list: pending_processes
                            .iter()
                            .map(|process| process.pid)
                            .collect(),
                        action,
                        backend: fallback_backend.to_owned(),
                        reason: format!(
                            "{}; primary_transaction={failure}; primary_rolled_back={}/{} rollback_failures={}",
                            fallback.reason,
                            freeze_outcome.rolled_back,
                            freeze_outcome.applied,
                            freeze_outcome.rollback_failures
                        ),
                        result,
                        details: format!(
                            "{} {}",
                            operation_details(&pending_processes),
                            fallback_details
                        ),
                    };
                    stamp_operation(&mut operation, state, timestamp_ms);
                    state.operation_log.push(operation);
                    if !state.frozen_apps.contains(&identity) {
                        state.frozen_since_ms.remove(&identity);
                    }
                    continue;
                }

                let post_freeze_processes = discover_processes(&fallback_package_name, record.uid)?;
                let original_pids = pending_processes
                    .iter()
                    .map(|process| process.pid)
                    .collect::<std::collections::BTreeSet<_>>();
                let new_pids = post_freeze_processes
                    .iter()
                    .filter(|process| {
                        process.uid == record.uid && !original_pids.contains(&process.pid)
                    })
                    .map(|process| process.pid)
                    .collect::<Vec<_>>();
                if !new_pids.is_empty() {
                    let rollback = unfreeze_identity_processes(
                        &package_name,
                        &post_freeze_processes,
                        FrozenOwnership::CgroupBinder,
                        &mut validate_process,
                        &mut unfreeze_process,
                    );
                    if rollback.failures.is_empty() {
                        state.clear_frozen(&identity);
                    } else {
                        // The process rescan invalidates the original freeze transaction. Keep
                        // the identity tracked until every attempted thaw has actually succeeded.
                        state.track_frozen(
                            identity.clone(),
                            timestamp_ms,
                            FrozenOwnership::ResidualUnknown,
                        );
                    }
                    state.pending_freezes.remove(&identity);
                    let mut operation = ControlOperation {
                        operation_id: 0,
                        timestamp_ms: 0,
                        package_name,
                        uid: record.uid,
                        pid_list: post_freeze_processes
                            .iter()
                            .map(|process| process.pid)
                            .collect(),
                        action: ControlAction::Freeze,
                        backend: backend_name(&post_freeze_processes).to_owned(),
                        reason: format!(
                            "{}; new same-uid process appeared after freeze",
                            decision.reason
                        ),
                        result: OperationResult::Partial,
                        details: format!(
                            "{} new_pids={new_pids:?} rollback_applied={} rollback_failures={}",
                            operation_details(&post_freeze_processes),
                            rollback.applied,
                            if rollback.failures.is_empty() {
                                "none".to_owned()
                            } else {
                                rollback.failures.join("; ")
                            }
                        ),
                    };
                    stamp_operation(&mut operation, state, timestamp_ms);
                    state.operation_log.push(operation);
                    if !state.frozen_apps.contains(&identity) {
                        state.frozen_since_ms.remove(&identity);
                    }
                    continue;
                }
                state.track_frozen(
                    identity.clone(),
                    timestamp_ms,
                    FrozenOwnership::CgroupBinder,
                );
                if mode_has_network_restriction(record.mode) {
                    state.network_restriction_requests.insert(record.uid);
                }
                state
                    .pending_freezes
                    .remove(&(package_name.clone(), record.uid));
                (ControlAction::Freeze, OperationResult::Success)
            }
            DecisionAction::Postpone => (ControlAction::Postpone, OperationResult::Postponed),
            DecisionAction::AlternateFreezer => (ControlAction::Fallback, OperationResult::Failed),
            DecisionAction::Signal => {
                let ledger_promotion = signal_stop_ledger_promotion(
                    state,
                    &identity,
                    &pending_processes,
                    &mut |pid, start_time_ticks| {
                        procfs::recorded_freezeit_signal_stop_ownership(pid, start_time_ticks)
                    },
                );
                let direct_signal_ownership = ledger_promotion.frozen_ownership();
                let outcome = apply_signal_stop_freeze_transaction(
                    &pending_processes,
                    &mut validate_process,
                    &mut freeze_process,
                    &mut unfreeze_process,
                    ledger_promotion,
                    &mut |process| match process.start_time_ticks {
                        Some(start_time_ticks) => {
                            procfs::promote_freezeit_signal_stop(process.pid, start_time_ticks)
                                .map(|_| ())
                        }
                        None => Ok(()),
                    },
                );
                if let Some(failure) = outcome.failure {
                    if failure.contains("shared uid contains multiple package identities") {
                        state
                            .blocked_process_signatures
                            .insert(identity.clone(), process_signature.clone());
                    }
                    if outcome.rollback_failures > 0 {
                        state.track_frozen(
                            identity.clone(),
                            timestamp_ms,
                            FrozenOwnership::ResidualUnknown,
                        );
                    }
                    let mut operation = ControlOperation {
                        operation_id: 0,
                        timestamp_ms: 0,
                        package_name,
                        uid: record.uid,
                        pid_list: pending_processes
                            .iter()
                            .map(|process| process.pid)
                            .collect(),
                        action: ControlAction::Freeze,
                        backend: "signal.stop".to_owned(),
                        reason: format!("{}; {failure}", decision.reason),
                        result: if outcome.rollback_failures == 0 {
                            OperationResult::Failed
                        } else {
                            OperationResult::Partial
                        },
                        details: format!(
                            "{} signaled={} rolled_back={} rollback_failures={}",
                            operation_details(&pending_processes),
                            outcome.applied,
                            outcome.rolled_back,
                            outcome.rollback_failures
                        ),
                    };
                    stamp_operation(&mut operation, state, timestamp_ms);
                    state.operation_log.push(operation);
                    state.pending_freezes.remove(&identity);
                    if !state.frozen_apps.contains(&identity) {
                        state.frozen_since_ms.remove(&identity);
                    }
                    continue;
                }
                state.track_frozen(identity.clone(), timestamp_ms, direct_signal_ownership);
                if mode_has_network_restriction(record.mode) {
                    state.network_restriction_requests.insert(record.uid);
                }
                state
                    .pending_freezes
                    .remove(&(package_name.clone(), record.uid));
                (ControlAction::Freeze, OperationResult::Success)
            }
            DecisionAction::Terminate => {
                let outcome = apply_terminate_transaction(
                    &pending_processes,
                    &mut validate_process,
                    &mut terminate_process,
                );
                if let Some(failure) = outcome.failure {
                    let mut operation = ControlOperation {
                        operation_id: 0,
                        timestamp_ms: 0,
                        package_name,
                        uid: record.uid,
                        pid_list: pending_processes
                            .iter()
                            .map(|process| process.pid)
                            .collect(),
                        action: ControlAction::Terminate,
                        backend: "signal.kill".to_owned(),
                        reason: format!("{}; {failure}", decision.reason),
                        result: if outcome.applied == 0 {
                            OperationResult::Failed
                        } else {
                            OperationResult::Partial
                        },
                        details: format!(
                            "{} sigkill_applied={}",
                            operation_details(&pending_processes),
                            outcome.applied
                        ),
                    };
                    stamp_operation(&mut operation, state, timestamp_ms);
                    state.operation_log.push(operation);
                    continue;
                }
                state.pending_freezes.remove(&identity);
                state.clear_frozen(&identity);
                (ControlAction::Terminate, OperationResult::Success)
            }
            DecisionAction::Skip => (ControlAction::Skip, OperationResult::Skipped),
        };

        if !state.frozen_apps.contains(&identity) {
            state.frozen_since_ms.remove(&identity);
        }

        let operation_backend = match decision.action {
            DecisionAction::Signal => "signal.stop",
            _ => requested_backend.operation_backend(&pending_processes),
        };
        let mut operation = ControlOperation {
            operation_id: 0,
            timestamp_ms: 0,
            package_name,
            uid: record.uid,
            pid_list: pending_processes
                .iter()
                .map(|process| process.pid)
                .collect(),
            action,
            backend: operation_backend.to_owned(),
            reason: if abnormal_thaw {
                format!("abnormal thaw audit; {}", decision.reason)
            } else {
                decision.reason
            },
            result,
            details: operation_details(&pending_processes),
        };
        stamp_operation(&mut operation, state, timestamp_ms);
        state.operation_log.push(operation);
    }

    Ok(())
}

fn refreeze_audit_due(
    state: &mut RuntimeControlState,
    settings: &[u8],
    timestamp_ms: u128,
) -> bool {
    let started_at = *state.audit_started_at_ms.get_or_insert(timestamp_ms);
    let elapsed = timestamp_ms.saturating_sub(started_at);
    let interval_ms = if elapsed < 15 * 60_000 {
        Some(60_000)
    } else {
        match settings.get(6).copied().unwrap_or(0) {
            1 => Some(30 * 60_000),
            2 => Some(60 * 60_000),
            3 => Some(120 * 60_000),
            _ => None,
        }
    };
    let Some(interval_ms) = interval_ms else {
        // Do not leave this as `None`: get_or_insert on the next pass would recreate
        // the startup deadline, which is already long expired after the 15-minute phase.
        state.next_refreeze_audit_at_ms = None;
        return false;
    };
    let next = state
        .next_refreeze_audit_at_ms
        .get_or_insert(started_at + 60_000);
    if timestamp_ms < *next {
        return false;
    }
    state.next_refreeze_audit_at_ms = Some(timestamp_ms + interval_ms);
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FreezeTransactionOutcome {
    applied: usize,
    rolled_back: usize,
    rollback_failures: usize,
    residual_possible: bool,
    failure: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignalStopLedgerPromotion {
    Never,
    AfterCompletedDirectTransaction,
}

impl SignalStopLedgerPromotion {
    fn frozen_ownership(self) -> FrozenOwnership {
        match self {
            Self::Never => FrozenOwnership::ResidualUnknown,
            Self::AfterCompletedDirectTransaction => FrozenOwnership::SignalOnly,
        }
    }
}

fn signal_stop_ledger_promotion(
    state: &RuntimeControlState,
    identity: &RuntimeIdentity,
    processes: &[RuntimeProcess],
    recorded_ownership: &mut impl FnMut(
        i32,
        u64,
    ) -> Result<Option<procfs::SignalStopOwnership>, DaemonError>,
) -> SignalStopLedgerPromotion {
    if state.has_unresolved_freeze_ownership(identity) {
        return SignalStopLedgerPromotion::Never;
    }

    for process in processes {
        let Some(start_time_ticks) = process.start_time_ticks else {
            return SignalStopLedgerPromotion::Never;
        };
        match recorded_ownership(process.pid, start_time_ticks) {
            Ok(Some(procfs::SignalStopOwnership::ResidualUnknown)) | Err(_) => {
                return SignalStopLedgerPromotion::Never;
            }
            Ok(None | Some(procfs::SignalStopOwnership::SignalOnly)) => {}
        }
    }

    SignalStopLedgerPromotion::AfterCompletedDirectTransaction
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminateTransactionOutcome {
    applied: usize,
    failure: Option<String>,
}

fn terminate_runtime_process(process: &RuntimeProcess) -> Result<(), DaemonError> {
    if process.pid <= 0 {
        return Err(DaemonError::system("refusing to SIGKILL non-positive pid"));
    }
    // SAFETY: the PID was identity-validated immediately before this call; libc::kill
    // performs no memory access and SIGKILL is the configured terminate backend.
    let result = unsafe { libc::kill(process.pid, libc::SIGKILL) };
    if result == 0 {
        Ok(())
    } else {
        Err(DaemonError::from(std::io::Error::last_os_error()))
    }
}

fn apply_terminate_transaction(
    processes: &[RuntimeProcess],
    validate_process: &mut impl FnMut(&RuntimeProcess) -> Result<bool, DaemonError>,
    terminate_process: &mut impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
) -> TerminateTransactionOutcome {
    let expected_package = processes
        .first()
        .map(|process| process.package_name.as_str());
    for process in processes {
        if Some(process.package_name.as_str()) != expected_package {
            return TerminateTransactionOutcome {
                applied: 0,
                failure: Some(format!(
                    "shared uid contains multiple package identities; refusing pid {}",
                    process.pid
                )),
            };
        }
        match validate_process(process) {
            Ok(true) => {}
            Ok(false) => {
                return TerminateTransactionOutcome {
                    applied: 0,
                    failure: Some(format!(
                        "identity validation failed before SIGKILL for pid {}",
                        process.pid
                    )),
                };
            }
            Err(error) => {
                return TerminateTransactionOutcome {
                    applied: 0,
                    failure: Some(format!(
                        "identity validation failed before SIGKILL for pid {}: {error}",
                        process.pid
                    )),
                };
            }
        }
    }

    let mut applied = 0;
    for process in processes {
        match validate_process(process) {
            Ok(true) => {}
            Ok(false) => {
                return TerminateTransactionOutcome {
                    applied,
                    failure: Some(format!(
                        "identity validation failed immediately before SIGKILL for pid {}",
                        process.pid
                    )),
                };
            }
            Err(error) => {
                return TerminateTransactionOutcome {
                    applied,
                    failure: Some(format!(
                        "identity validation failed immediately before SIGKILL for pid {}: {error}",
                        process.pid
                    )),
                };
            }
        }
        if let Err(error) = terminate_process(process) {
            return TerminateTransactionOutcome {
                applied,
                failure: Some(format!("SIGKILL failed for pid {}: {error}", process.pid)),
            };
        }
        applied += 1;
    }
    TerminateTransactionOutcome {
        applied,
        failure: None,
    }
}

fn apply_freeze_transaction(
    processes: &[RuntimeProcess],
    validate_process: &mut impl FnMut(&RuntimeProcess) -> Result<bool, DaemonError>,
    freeze_process: &mut impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
    unfreeze_process: &mut impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
) -> FreezeTransactionOutcome {
    let expected_package = processes
        .first()
        .map(|process| process.package_name.as_str());
    for process in processes {
        if Some(process.package_name.as_str()) != expected_package {
            return FreezeTransactionOutcome {
                applied: 0,
                rolled_back: 0,
                rollback_failures: 0,
                residual_possible: false,
                failure: Some(format!(
                    "shared uid contains multiple package identities; refusing pid {}",
                    process.pid
                )),
            };
        }
        match validate_process(process) {
            Ok(true) => {}
            Ok(false) => {
                return FreezeTransactionOutcome {
                    applied: 0,
                    rolled_back: 0,
                    rollback_failures: 0,
                    residual_possible: false,
                    failure: Some(format!(
                        "identity validation failed for pid {}",
                        process.pid
                    )),
                };
            }
            Err(error) => {
                return FreezeTransactionOutcome {
                    applied: 0,
                    rolled_back: 0,
                    rollback_failures: 0,
                    residual_possible: false,
                    failure: Some(format!(
                        "identity validation failed for pid {}: {error}",
                        process.pid
                    )),
                };
            }
        }
    }

    let mut signaled = Vec::new();
    let mut failure = None;
    for process in processes {
        match validate_process(process) {
            Ok(true) => {}
            Ok(false) => {
                failure = Some(format!(
                    "identity validation failed for pid {}",
                    process.pid
                ));
                break;
            }
            Err(error) => {
                failure = Some(format!(
                    "identity validation failed for pid {}: {error}",
                    process.pid
                ));
                break;
            }
        }
        if let Err(error) = freeze_process(process) {
            failure = Some(format!(
                "control apply failed for pid {}: {error}",
                process.pid
            ));
            break;
        }
        signaled.push(process);
    }

    if failure.is_none() {
        return FreezeTransactionOutcome {
            applied: signaled.len(),
            rolled_back: 0,
            rollback_failures: 0,
            residual_possible: false,
            failure: None,
        };
    }

    let mut rolled_back = 0;
    let mut rollback_failures = 0;
    for process in signaled.iter().rev() {
        let identity_valid = validate_process(process).unwrap_or(false);
        if !identity_valid || unfreeze_process(process).is_err() {
            rollback_failures += 1;
        } else {
            rolled_back += 1;
        }
    }
    FreezeTransactionOutcome {
        applied: signaled.len(),
        rolled_back,
        rollback_failures,
        residual_possible: true,
        failure,
    }
}

/// Run a SIGSTOP transaction.  Promotion is allowed only for the direct signal
/// decision after every process was stopped successfully.  A generic freezer
/// failure may fall back to the same signal mechanism, but uses `Never` so its
/// default residual ledger provenance is retained across daemon restarts.
fn apply_signal_stop_freeze_transaction(
    processes: &[RuntimeProcess],
    validate_process: &mut impl FnMut(&RuntimeProcess) -> Result<bool, DaemonError>,
    freeze_process: &mut impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
    unfreeze_process: &mut impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
    promotion: SignalStopLedgerPromotion,
    promote_signal_stop: &mut impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
) -> FreezeTransactionOutcome {
    let outcome = apply_freeze_transaction(
        processes,
        validate_process,
        &mut |process| freeze_process(&signal_control_process(process)),
        &mut |process| unfreeze_process(&signal_control_process(process)),
    );
    if outcome.failure.is_none()
        && promotion == SignalStopLedgerPromotion::AfterCompletedDirectTransaction
    {
        for process in processes {
            // The live control state is already known to be signal-only.  If the
            // durable promotion cannot be persisted, leave its default ledger
            // provenance as ResidualUnknown so restart recovery stays conservative.
            let _ = promote_signal_stop(process);
        }
    }
    outcome
}

fn operation_details(processes: &[RuntimeProcess]) -> String {
    let mut details = format!("process_count={}", processes.len());
    let evidence = processes
        .iter()
        .filter_map(|process| {
            process
                .binder_state
                .as_ref()
                .map(|state| format!("pid{}:{state}", process.pid))
        })
        .collect::<Vec<_>>();
    if !evidence.is_empty() {
        details.push_str(" idle_evidence=");
        details.push_str(&evidence.join("|"));
    }
    details
}

pub fn is_control_policy_mode(mode: i32) -> bool {
    matches!(mode, 10 | 20 | 21 | 30 | 31)
}

fn backend_environment(processes: &[RuntimeProcess]) -> BackendEnvironment {
    // 测试钩子：binder freezer 在 CI/非 Android 环境检测必然不可用，但控制逻辑的
    // 单元测试需要模拟「cgroup+binder 均可用」的真实设备场景。生产路径该 thread_local
    // 始终为 None，走真实 binder 检测；仅测试通过 set_test_binder_available 注入。
    let binder_available = match TEST_BINDER_AVAILABLE.with(|cell| cell.get()) {
        Some(value) => value,
        None => discover_runtime_capabilities().iter().any(|capability| {
            capability.name == crate::domain::capability::CapabilityName::BinderFreezer
                && capability.status == crate::domain::capability::CapabilityStatus::Available
        }),
    };
    BackendEnvironment {
        cgroup_available: !processes.is_empty()
            && processes
                .iter()
                .all(|process| process.cgroup_freeze_path.is_some()),
        binder_available,
        network_available: true,
        wakelock_available: true,
        screen_state_available: true,
        hook_fresh: true,
    }
}

thread_local! {
    /// 测试专用：覆盖 binder 可用性检测结果。生产恒为 None。
    static TEST_BINDER_AVAILABLE: std::cell::Cell<Option<bool>> = std::cell::Cell::new(None);
}

/// 测试专用：注入 binder 可用性，返回一个 guard 在 drop 时恢复默认检测。
#[doc(hidden)]
pub fn set_test_binder_available(available: bool) -> impl Drop {
    TEST_BINDER_AVAILABLE.with(|cell| cell.set(Some(available)));
    struct ResetGuard;
    impl Drop for ResetGuard {
        fn drop(&mut self) {
            TEST_BINDER_AVAILABLE.with(|cell| cell.set(None));
        }
    }
    ResetGuard
}

fn backend_name(processes: &[RuntimeProcess]) -> &'static str {
    if !processes.is_empty()
        && processes
            .iter()
            .all(|process| process.cgroup_freeze_path.is_some())
    {
        "cgroup.freeze"
    } else {
        "signal-control-pass"
    }
}

fn stamp_operation(
    operation: &mut ControlOperation,
    state: &mut RuntimeControlState,
    timestamp_ms: u128,
) {
    operation.operation_id = state.next_operation_id;
    state.next_operation_id += 1;
    operation.timestamp_ms = timestamp_ms;
}

fn managed_app_from_record(package_name: &str, uid: u32) -> ManagedApp {
    const PER_USER_RANGE: u32 = 100_000;
    const FIRST_APPLICATION_UID: u32 = 10_000;
    let is_system_app = uid % PER_USER_RANGE < FIRST_APPLICATION_UID;
    ManagedApp {
        package_name: package_name.to_owned(),
        user_id: 0,
        uid,
        label: package_name.to_owned(),
        is_system_app,
        protected_reason: protected_reason_for(package_name, is_system_app)
            .or(is_system_app.then_some(ProtectedReason::SystemCritical)),
        policy_id: "manager-v1".to_owned(),
        last_seen_baseline: "runtime".to_owned(),
    }
}

fn policy_from_record(record: &ManagerAppConfigRecord) -> FreezePolicy {
    let mode = match record.mode {
        10 => FreezeMode::Terminate,
        20 | 30 => FreezeMode::Freeze,
        21 | 31 => FreezeMode::FreezeWithRestrictions,
        40 | 50 => FreezeMode::Protected,
        _ => FreezeMode::Free,
    };

    FreezePolicy::Selected {
        mode,
        delay_ms: 0,
        foreground_strategy: if record.permissive {
            ForegroundStrategy::Permissive
        } else {
            ForegroundStrategy::Strict
        },
        allow_network_restriction: mode_has_network_restriction(record.mode),
        allow_wakelock_restriction: false,
        fallback_strategy: vec![FallbackAction::Signal, FallbackAction::Skip],
        updated_at_ms: 0,
    }
}

fn manager_policy_delay_ms(record: &ManagerAppConfigRecord, settings: &[u8]) -> u64 {
    let seconds = match record.mode {
        10 => settings.get(4).copied().unwrap_or(0),
        20 | 21 | 30 | 31 => settings.get(2).copied().unwrap_or(0),
        _ => 0,
    };
    u64::from(seconds) * 1000
}

fn operation_from_fallback(action: DecisionAction) -> (ControlAction, OperationResult) {
    match action {
        DecisionAction::Freeze => (ControlAction::Freeze, OperationResult::Success),
        DecisionAction::Postpone => (ControlAction::Postpone, OperationResult::Postponed),
        DecisionAction::AlternateFreezer | DecisionAction::Signal => {
            (ControlAction::Fallback, OperationResult::Skipped)
        }
        DecisionAction::Terminate => (ControlAction::Terminate, OperationResult::Failed),
        DecisionAction::Skip => (ControlAction::Skip, OperationResult::Skipped),
    }
}

pub fn run_manager_server_once(state: &ReadOnlyState) -> Result<(), DaemonError> {
    let listener = socket::bind_manager_listener()?;
    let (stream, _) = listener.accept()?;
    let mut state = state.clone();
    socket::handle_single_manager_stream(stream, &mut state)
}

pub fn load_policy_with_retries(
    paths: &DaemonPaths,
    attempts: usize,
) -> Result<LoadedPolicyFiles, DaemonError> {
    let attempts = attempts.max(1);
    let mut last_result = None;

    for _ in 0..attempts {
        let loaded = load_policy_files(paths)?;
        if loaded.is_available() {
            return Ok(loaded);
        }
        last_result = Some(loaded);
    }

    Ok(last_result.unwrap_or(LoadedPolicyFiles {
        app_config: None,
        app_label: None,
        settings: None,
    }))
}

pub fn decide_freeze(
    app: &crate::domain::policy::ManagedApp,
    policy: &crate::domain::policy::FreezePolicy,
    processes: &[crate::domain::runtime::RuntimeProcess],
) -> FreezeDecision {
    SystemAwareCgroupBinderBackend::new(BackendEnvironment::default())
        .can_freeze(app, policy, processes)
}

pub fn decide_freeze_after_reconciliation(
    app: &crate::domain::policy::ManagedApp,
    current_package: &PackageRecord,
    policy: &crate::domain::policy::FreezePolicy,
    processes: &[crate::domain::runtime::RuntimeProcess],
) -> Result<FreezeDecision, DaemonError> {
    reconcile_uid(app, current_package).map_err(DaemonError::system)?;
    Ok(decide_freeze(app, policy, processes))
}

pub fn mark_frozen(processes: &mut [crate::domain::runtime::RuntimeProcess]) {
    mark_processes_frozen(processes);
}

pub fn mark_running(processes: &mut [crate::domain::runtime::RuntimeProcess]) {
    mark_processes_running(processes);
}

pub fn recover_after_restart(
    operation_id: u64,
    timestamp_ms: u128,
    package_name: &str,
    uid: u32,
    processes: &[crate::domain::runtime::RuntimeProcess],
) -> ControlOperation {
    ControlOperation {
        operation_id,
        timestamp_ms,
        package_name: package_name.to_owned(),
        uid,
        pid_list: processes.iter().map(|process| process.pid).collect(),
        action: ControlAction::Recover,
        backend: "restart-reconciliation".to_owned(),
        reason: "daemon restart reconciliation".to_owned(),
        result: OperationResult::Success,
        details: format!(
            "observed {} process(es) before new control",
            processes.len()
        ),
    }
}

#[derive(Debug, Default)]
struct RestartRecoveryOutcome {
    cgroup_thawed: bool,
    signal_resumed: bool,
    failures: Vec<String>,
}

fn restart_identity_is_current(
    process: &RuntimeProcess,
    phase: &str,
    validate_process: &mut impl FnMut(&RuntimeProcess) -> Result<bool, DaemonError>,
) -> Result<(), String> {
    match validate_process(process) {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!(
            "identity validation failed {phase} for pid {}",
            process.pid
        )),
        Err(error) => Err(format!(
            "identity validation failed {phase} for pid {}: {error}",
            process.pid
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn recover_process_after_restart(
    process: &RuntimeProcess,
    signal_stop_ownership: Option<procfs::SignalStopOwnership>,
    observed_cgroup_state: Option<cgroup::FreezeState>,
    validate_process: &mut impl FnMut(&RuntimeProcess) -> Result<bool, DaemonError>,
    thaw_cgroup: &mut impl FnMut(&str) -> Result<(), DaemonError>,
    unfreeze_binder: &mut impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
    resume_signal: &mut impl FnMut(&RuntimeProcess) -> Result<(), DaemonError>,
) -> RestartRecoveryOutcome {
    let mut outcome = RestartRecoveryOutcome::default();
    let signal_only_recovery = signal_stop_ownership
        == Some(procfs::SignalStopOwnership::SignalOnly)
        && observed_cgroup_state == Some(cgroup::FreezeState::Thawed);
    if let Some(cgroup_path) = process
        .cgroup_freeze_path
        .as_deref()
        .filter(|_| !signal_only_recovery)
    {
        if let Err(error) =
            restart_identity_is_current(process, "before cgroup thaw", validate_process)
        {
            outcome.failures.push(error);
            return outcome;
        }
        match thaw_cgroup(cgroup_path) {
            Ok(()) => outcome.cgroup_thawed = true,
            Err(error) => outcome.failures.push(format!(
                "cgroup thaw failed for pid {}: {error}",
                process.pid
            )),
        }

        if let Err(error) =
            restart_identity_is_current(process, "before binder unfreeze", validate_process)
        {
            outcome.failures.push(error);
            return outcome;
        }
        if let Err(error) = unfreeze_binder(process) {
            outcome.failures.push(format!(
                "binder unfreeze failed for pid {}: {error}",
                process.pid
            ));
        }
    } else if signal_stop_ownership.is_none() {
        return outcome;
    }

    if signal_stop_ownership.is_some() {
        if let Err(error) =
            restart_identity_is_current(process, "immediately before SIGCONT", validate_process)
        {
            outcome.failures.push(error);
            return outcome;
        }
        match resume_signal(process) {
            Ok(()) => outcome.signal_resumed = true,
            Err(error) => outcome
                .failures
                .push(format!("SIGCONT failed for pid {}: {error}", process.pid)),
        }
    }
    outcome
}

/// 守护进程重启后，上一轮用信号回退（SIGSTOP）冻结的进程仍处于 T 状态：
/// `frozen_apps` 仅存内存未持久化，新实例不知道哪些进程曾被 SIGSTOP，
/// discover 又把 control_state 硬编码为 Running，导致前台解冻路径不会发 SIGCONT。
/// 这里在进入 server 前，逐个重验 PID/UID/start-time。只有 ledger 明确标为
/// `SignalOnly` 且 cgroup 实测已 thawed 时才只补发 SIGCONT；旧格式、未知来源、
/// cgroup frozen 或无法观测时均执行完整的 cgroup/binder 清理，让这些应用能被正常
/// 使用后再由控制循环决定是否冻结。
pub fn recover_stopped_managed_processes_after_restart(state: &mut ReadOnlyState) {
    let managed_uids: BTreeSet<u32> = state
        .app_config
        .iter()
        .filter(|record| is_control_policy_mode(record.mode))
        .map(|record| record.uid)
        .collect();
    if managed_uids.is_empty() {
        return;
    }

    let timestamp_ms = current_timestamp_ms();
    let processes_by_uid =
        match procfs::discover_managed_uid_processes(procfs::PROC_ROOT, &managed_uids) {
            Ok(map) => map,
            Err(error) => {
                state.manager_log.push_once(LogRecord::fault(
                    LogLevel::Error,
                    timestamp_ms,
                    format!("restart recovery: process discovery failed: {error}"),
                ));
                return;
            }
        };

    let mut cgroup_thawed = 0u32;
    let mut signal_resumed = 0u32;
    let mut failed = 0u32;
    for (_uid, processes) in &processes_by_uid {
        for process in processes {
            let signal_stop_ownership =
                fs::read_to_string(format!("{}/{}/stat", procfs::PROC_ROOT, process.pid))
                    .ok()
                    .and_then(|stat| procfs::freezeit_signal_stop_ownership(&stat));
            let observed_cgroup_state = process
                .cgroup_freeze_path
                .as_deref()
                .and_then(|path| cgroup::read_freeze_state(path).ok());
            if process.cgroup_freeze_path.is_none() && signal_stop_ownership.is_none() {
                continue;
            }

            let outcome = recover_process_after_restart(
                process,
                signal_stop_ownership,
                observed_cgroup_state,
                &mut |candidate| procfs::recheck_process_identity(procfs::PROC_ROOT, candidate),
                &mut |path| cgroup::write_freeze_state(path, cgroup::FreezeState::Thawed),
                &mut |candidate| {
                    let binder_pid = u32::try_from(candidate.pid).map_err(|_| {
                        DaemonError::system(format!("invalid binder pid {}", candidate.pid))
                    })?;
                    let binder_path = binder::discover_binder_device().ok_or_else(|| {
                        DaemonError::system("binder device disappeared during restart recovery")
                    })?;
                    Ok(binder::set_binder_freeze(
                        binder_path,
                        binder_pid,
                        binder::BinderFreezeRequest::Unfreeze,
                        0,
                    )?)
                },
                &mut |candidate| signal::send_signal(candidate.pid, signal::SignalAction::Continue),
            );
            if outcome.cgroup_thawed {
                cgroup_thawed += 1;
            }
            if outcome.signal_resumed {
                signal_resumed += 1;
            }
            if !outcome.failures.is_empty() {
                failed += 1;
            }
        }
    }

    if cgroup_thawed > 0 || signal_resumed > 0 || failed > 0 {
        state.manager_log.push_once(LogRecord::fault(
            LogLevel::Info,
            timestamp_ms,
            format!(
                "restart recovery: cgroup_thawed={cgroup_thawed} sigcont_resumed={signal_resumed} failed={failed}"
            ),
        ));
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

    use super::{
        apply_signal_stop_freeze_transaction, managed_app_from_record, manager_wakeup_interval_ms,
        parse_legacy_uid_token, policy_from_record, reconcile_live_process_control_states,
        recover_process_after_restart, refreeze_audit_due, requested_control_backend,
        run_control_pass, run_control_pass_with_sampling_and_terminate,
        run_control_pass_with_settings, signal_stop_ledger_promotion, FrozenOwnership,
        RequestedControlBackend, RuntimeControlState, SignalStopLedgerPromotion,
    };
    use crate::{
        app::{controller::set_test_binder_available, error::DaemonError},
        domain::{
            policy::ProtectedReason,
            runtime::{ControlState, ProcessState, RuntimeProcess},
        },
        protocol::manager_v1::ManagerAppConfigRecord,
        sys::{cgroup::FreezeState, procfs::SignalStopOwnership},
    };

    fn process(uid: u32, pid: i32, control_state: ControlState) -> RuntimeProcess {
        RuntimeProcess {
            pid,
            uid,
            package_name: "com.example.app".to_owned(),
            process_name: "com.example.app".to_owned(),
            proc_state: ProcessState::Cached,
            control_state,
            cgroup_freeze_path: None,
            binder_state: None,
            start_time_ticks: Some(1),
            last_seen_at_ms: 0,
        }
    }

    #[test]
    fn live_control_state_reconciliation_uses_cgroup_and_stop_evidence() {
        let uid = 10_123;
        let mut cgroup_frozen = process(uid, 123, ControlState::Running);
        cgroup_frozen.cgroup_freeze_path = Some("/cgroup/frozen".to_owned());
        let stopped = process(uid, 124, ControlState::Running);
        let tracing_stopped = process(uid, 125, ControlState::Running);
        let mut cgroup_thawed = process(uid, 126, ControlState::Frozen);
        cgroup_thawed.cgroup_freeze_path = Some("/cgroup/thawed".to_owned());
        let mut processes = vec![cgroup_frozen, stopped, tracing_stopped, cgroup_thawed];

        reconcile_live_process_control_states(
            &mut processes,
            |path| match path {
                "/cgroup/frozen" => Ok(FreezeState::Frozen),
                "/cgroup/thawed" => Ok(FreezeState::Thawed),
                unexpected => Err(DaemonError::system(format!(
                    "unexpected cgroup path {unexpected}"
                ))),
            },
            |pid| match pid {
                123 | 126 => Ok('S'),
                124 => Ok('T'),
                125 => Ok('t'),
                unexpected => Err(DaemonError::system(format!("unexpected pid {unexpected}"))),
            },
        );

        assert_eq!(processes[0].control_state, ControlState::Frozen);
        assert_eq!(processes[1].control_state, ControlState::Frozen);
        assert_eq!(processes[2].control_state, ControlState::Frozen);
        assert_eq!(processes[3].control_state, ControlState::Running);
    }

    #[test]
    fn live_control_state_reconciliation_keeps_uncertain_processes_unknown() {
        let uid = 10_123;
        let mut unreadable_cgroup = process(uid, 123, ControlState::Running);
        unreadable_cgroup.cgroup_freeze_path = Some("/cgroup/unreadable".to_owned());
        let unreadable_proc = process(uid, 124, ControlState::Running);
        let mut processes = vec![unreadable_cgroup, unreadable_proc];

        reconcile_live_process_control_states(
            &mut processes,
            |_| Err(DaemonError::system("cgroup state unavailable")),
            |pid| match pid {
                123 => Ok('S'),
                124 => Err(DaemonError::system("proc state unavailable")),
                unexpected => Err(DaemonError::system(format!("unexpected pid {unexpected}"))),
            },
        );

        assert_eq!(processes[0].control_state, ControlState::Unknown);
        assert_eq!(processes[1].control_state, ControlState::Unknown);
    }

    #[test]
    fn refreeze_audit_keeps_unknown_processes_tracked_without_refreezing() {
        let uid = 10_123;
        let mut state = RuntimeControlState::default();
        let freeze_calls = RefCell::new(0);

        run_control_pass(
            &mut state,
            &[config(uid, 30)],
            |_, _| Ok(vec![process(uid, 123, ControlState::Running)]),
            |_| {
                *freeze_calls.borrow_mut() += 1;
                Ok(())
            },
            |_| Ok(()),
            &[],
            0,
        )
        .expect("initial freeze succeeds");

        run_control_pass(
            &mut state,
            &[config(uid, 30)],
            |_, _| Ok(vec![process(uid, 123, ControlState::Unknown)]),
            |_| {
                *freeze_calls.borrow_mut() += 1;
                Ok(())
            },
            |_| Ok(()),
            &[],
            60_000,
        )
        .expect("uncertain audit observation is not treated as a thaw");

        assert_eq!(*freeze_calls.borrow(), 1);
        let rows = state.freeze_status_records(
            &[config(uid, 30)],
            &BTreeMap::from([(uid, vec![process(uid, 123, ControlState::Unknown)])]),
            &[],
            60_000,
        );
        assert_eq!(rows[0].state, 3, "unknown state remains tracked as frozen");
    }

    fn config(uid: u32, mode: i32) -> ManagerAppConfigRecord {
        ManagerAppConfigRecord {
            uid,
            mode,
            permissive: false,
        }
    }

    #[test]
    fn legacy_uid_tokens_require_a_canonical_numeric_format() {
        assert_eq!(parse_legacy_uid_token("10000"), Some(10_000));
        assert_eq!(parse_legacy_uid_token("10000uid10000"), Some(10_000));
        assert_eq!(parse_legacy_uid_token("110000uid110000"), Some(110_000));
        assert_eq!(parse_legacy_uid_token("com.example.uid1000"), None);
        assert_eq!(parse_legacy_uid_token("10000uid10001"), None);
    }

    #[test]
    fn runtime_system_uids_are_protected_from_control() {
        let app = managed_app_from_record("android", 1_000);

        assert!(app.is_system_app);
        assert_eq!(app.protected_reason, Some(ProtectedReason::SystemCritical));
    }

    #[test]
    fn system_uid_signal_policy_is_skipped_before_any_signal_is_sent() {
        let uid = 1_000;
        let mut state = RuntimeControlState::default();
        let mut settings = vec![0; 256];
        settings[2] = 10;
        let signal_calls = RefCell::new(0);

        run_control_pass_with_settings(
            &mut state,
            &[config(uid, 20)],
            &settings,
            |_, _| Ok(vec![process(uid, 123, ControlState::Running)]),
            |_| {
                *signal_calls.borrow_mut() += 1;
                Ok(())
            },
            |_| Ok(()),
            &[],
            0,
        )
        .expect("system uid policy is evaluated without a backend error");

        assert_eq!(*signal_calls.borrow(), 0);
        assert!(
            state.pending_freeze_uids().is_empty(),
            "protected system UID must not be exposed to the hook as pending"
        );
    }

    #[test]
    fn protected_system_uid_thaws_historical_freeze_tracking() {
        let uid = 1_000;
        let mut state = RuntimeControlState::default();
        state.track_frozen(
            ("com.example.app".to_owned(), uid),
            0,
            FrozenOwnership::ResidualUnknown,
        );
        let unfreezes = RefCell::new(Vec::new());

        run_control_pass(
            &mut state,
            &[config(uid, 20)],
            |_, _| Ok(vec![process(uid, 123, ControlState::Frozen)]),
            |_| panic!("protected system UID must never freeze again"),
            |process| {
                unfreezes.borrow_mut().push(process.pid);
                Ok(())
            },
            &[],
            1,
        )
        .expect("protected system UID recovery");

        assert_eq!(&*unfreezes.borrow(), &[123]);
        assert!(state.pending_freeze_uids().is_empty());
    }

    #[test]
    fn tracking_signal_does_not_downgrade_legacy_frozen_identity() {
        let identity = ("com.example.app".to_owned(), 10_123);
        let mut state = RuntimeControlState::default();
        state.frozen_apps.insert(identity.clone());

        state.track_frozen(identity.clone(), 0, FrozenOwnership::SignalOnly);

        assert_eq!(
            state.frozen_ownership(&identity),
            FrozenOwnership::ResidualUnknown
        );
    }

    #[test]
    fn detached_audit_ownership_is_cleared_when_the_process_exits() {
        let uid = 10_123;
        let identity = ("com.example.app".to_owned(), uid);
        let mut state = RuntimeControlState::default();
        state
            .frozen_ownership
            .insert(identity.clone(), FrozenOwnership::ResidualUnknown);

        run_control_pass(
            &mut state,
            &[config(uid, 30)],
            |_, _| Ok(Vec::new()),
            |_| panic!("an exited process must not be frozen"),
            |_| panic!("an exited process must not be thawed"),
            &[],
            0,
        )
        .expect("process exit cleanup");

        assert!(!state.frozen_ownership.contains_key(&identity));
    }

    #[test]
    fn detached_audit_ownership_is_cleared_when_control_is_removed() {
        let identity = ("com.example.app".to_owned(), 10_123);
        let mut state = RuntimeControlState::default();
        state
            .frozen_ownership
            .insert(identity.clone(), FrozenOwnership::ResidualUnknown);

        run_control_pass(
            &mut state,
            &[],
            |_, _| panic!("removed control must not rediscover detached ownership"),
            |_| panic!("removed control must not refreeze detached ownership"),
            |_| panic!("removed control must not thaw an already-running process"),
            &[],
            0,
        )
        .expect("removed control cleanup");

        assert!(!state.frozen_ownership.contains_key(&identity));
    }

    #[test]
    fn disabling_control_clears_detached_audit_ownership() {
        let identity = ("com.example.app".to_owned(), 10_123);
        let mut state = RuntimeControlState::default();
        state
            .frozen_ownership
            .insert(identity.clone(), FrozenOwnership::ResidualUnknown);

        state
            .thaw_all_frozen(
                |_, _| panic!("detached ownership has no frozen process to rediscover"),
                |_| panic!("detached ownership has no frozen process to thaw"),
                |_| panic!("detached ownership has no frozen process to validate"),
                0,
            )
            .expect("disabled control cleanup");

        assert!(!state.frozen_ownership.contains_key(&identity));
    }

    #[test]
    fn sigstop_mode_uses_the_signal_backend_even_when_cgroup_is_available() {
        let _binder_guard = set_test_binder_available(true);
        let uid = 10_123;
        let mut process = process(uid, 123, ControlState::Running);
        process.cgroup_freeze_path =
            Some("/sys/fs/cgroup/uid_10123/pid_123/cgroup.freeze".to_owned());
        let signal_backend_was_used = RefCell::new(false);

        run_control_pass(
            &mut RuntimeControlState::default(),
            &[config(uid, 20)],
            |_, _| Ok(vec![process.clone()]),
            |candidate| {
                *signal_backend_was_used.borrow_mut() = candidate.cgroup_freeze_path.is_none();
                Ok(())
            },
            |_| Ok(()),
            &[],
            0,
        )
        .expect("SIGSTOP control pass");

        assert!(*signal_backend_was_used.borrow());
    }

    #[test]
    fn freezer_fallback_to_signal_reports_the_signal_backend() {
        let _binder_guard = set_test_binder_available(false);
        let uid = 10_123;
        let mut runtime_process = process(uid, 123, ControlState::Running);
        runtime_process.cgroup_freeze_path =
            Some("/sys/fs/cgroup/uid_10123/pid_123/cgroup.freeze".to_owned());

        let mut state = RuntimeControlState::default();
        run_control_pass(
            &mut state,
            &[config(uid, 30)],
            |_, _| Ok(vec![runtime_process.clone()]),
            |candidate| {
                assert!(candidate.cgroup_freeze_path.is_none());
                Ok(())
            },
            |_| Ok(()),
            &[],
            0,
        )
        .expect("signal fallback succeeds");

        let json = state.operation_log.to_json();
        assert!(json.contains("\"backend\":\"signal.stop\""));
        assert!(!json.contains("\"backend\":\"cgroup.freeze\""));
    }

    #[test]
    fn binderless_signal_fallback_foreground_thaw_allows_immediate_refreeze() {
        let _binder_guard = set_test_binder_available(false);
        let uid = 10_123;
        let mut runtime_process = process(uid, 123, ControlState::Running);
        runtime_process.cgroup_freeze_path =
            Some("/sys/fs/cgroup/uid_10123/pid_123/cgroup.freeze".to_owned());
        let mut state = RuntimeControlState::default();
        let stop_pids = Rc::new(RefCell::new(Vec::new()));
        let foreground_paths = Rc::new(RefCell::new(Vec::new()));

        run_control_pass(
            &mut state,
            &[config(uid, 30)],
            |_, _| Ok(vec![runtime_process.clone()]),
            {
                let stop_pids = Rc::clone(&stop_pids);
                move |candidate| {
                    assert!(candidate.cgroup_freeze_path.is_none());
                    stop_pids.borrow_mut().push(candidate.pid);
                    Ok(())
                }
            },
            |_| Ok(()),
            &[],
            0,
        )
        .expect("binderless fallback freezes with SIGSTOP");

        run_control_pass(
            &mut state,
            &[config(uid, 30)],
            |_, _| Ok(vec![runtime_process.clone()]),
            |_| panic!("foreground process must not be frozen"),
            {
                let foreground_paths = Rc::clone(&foreground_paths);
                move |candidate| {
                    foreground_paths
                        .borrow_mut()
                        .push(candidate.cgroup_freeze_path.clone());
                    if candidate.cgroup_freeze_path.is_some() {
                        Err(DaemonError::system("binder unfreeze failed after SIGCONT"))
                    } else {
                        Ok(())
                    }
                }
            },
            &[uid],
            1,
        )
        .expect("foreground signal thaw is handled by the control pass");

        run_control_pass(
            &mut state,
            &[config(uid, 30)],
            |_, _| Ok(vec![runtime_process.clone()]),
            {
                let stop_pids = Rc::clone(&stop_pids);
                move |candidate| {
                    assert!(candidate.cgroup_freeze_path.is_none());
                    stop_pids.borrow_mut().push(candidate.pid);
                    Ok(())
                }
            },
            |_| Ok(()),
            &[],
            2,
        )
        .expect("background control pass");

        assert_eq!(&*stop_pids.borrow(), &[123, 123]);
        assert_eq!(&*foreground_paths.borrow(), &[None]);
        assert!(state
            .operation_log
            .to_json()
            .contains("\"backend\":\"signal.cont\""));
    }

    #[test]
    fn primary_freezer_failure_signal_fallback_retains_generic_thaw_ownership() {
        let _binder_guard = set_test_binder_available(true);
        let uid = 10_123;
        let mut runtime_process = process(uid, 9_999_999, ControlState::Running);
        runtime_process.cgroup_freeze_path =
            Some("/sys/fs/cgroup/uid_10123/pid_9999999/cgroup.freeze".to_owned());
        let identity = ("com.example.app".to_owned(), uid);
        let mut state = RuntimeControlState::default();
        let signal_fallback_pids = Rc::new(RefCell::new(Vec::new()));
        let foreground_paths = Rc::new(RefCell::new(Vec::new()));

        run_control_pass(
            &mut state,
            &[config(uid, 30)],
            |_, _| Ok(vec![runtime_process.clone()]),
            {
                let signal_fallback_pids = Rc::clone(&signal_fallback_pids);
                move |candidate| {
                    if candidate.cgroup_freeze_path.is_some() {
                        Err(DaemonError::system("primary cgroup/binder freezer failed"))
                    } else {
                        signal_fallback_pids.borrow_mut().push(candidate.pid);
                        Ok(())
                    }
                }
            },
            |_| Ok(()),
            &[],
            0,
        )
        .expect("signal fallback succeeds through the injected signal backend");

        run_control_pass(
            &mut state,
            &[config(uid, 30)],
            |_, _| Ok(vec![runtime_process.clone()]),
            |_| panic!("foreground process must not be frozen"),
            {
                let foreground_paths = Rc::clone(&foreground_paths);
                move |candidate| {
                    foreground_paths
                        .borrow_mut()
                        .push(candidate.cgroup_freeze_path.clone());
                    Err(DaemonError::system("binder unfreeze failed after SIGCONT"))
                }
            },
            &[uid],
            1,
        )
        .expect("foreground generic thaw failure is logged without aborting the pass");

        run_control_pass(
            &mut state,
            &[config(uid, 30)],
            |_, _| Ok(vec![runtime_process.clone()]),
            |_| panic!("retained generic ownership must block immediate refreeze"),
            |_| Ok(()),
            &[],
            2,
        )
        .expect("retained ownership blocks immediate refreeze");

        assert_eq!(&*signal_fallback_pids.borrow(), &[9_999_999]);
        assert_eq!(
            &*foreground_paths.borrow(),
            &[Some(
                "/sys/fs/cgroup/uid_10123/pid_9999999/cgroup.freeze".to_owned()
            )]
        );
        assert!(state.frozen_apps.contains(&identity));
        assert_eq!(
            state.frozen_ownership(&identity),
            FrozenOwnership::ResidualUnknown
        );
    }

    #[test]
    fn partial_primary_freezer_failure_signal_fallback_keeps_generic_ownership() {
        let _binder_guard = set_test_binder_available(true);
        let uid = 10_123;
        let mut first = process(uid, 123, ControlState::Running);
        first.cgroup_freeze_path =
            Some("/sys/fs/cgroup/uid_10123/pid_123/cgroup.freeze".to_owned());
        let mut second = process(uid, 456, ControlState::Running);
        second.cgroup_freeze_path =
            Some("/sys/fs/cgroup/uid_10123/pid_456/cgroup.freeze".to_owned());
        let processes = vec![first.clone(), second.clone()];
        let identity = ("com.example.app".to_owned(), uid);
        let mut state = RuntimeControlState::default();
        let signal_fallback_pids = Rc::new(RefCell::new(Vec::new()));
        let foreground_paths = Rc::new(RefCell::new(Vec::new()));

        run_control_pass(
            &mut state,
            &[config(uid, 30)],
            |_, _| Ok(processes.clone()),
            {
                let signal_fallback_pids = Rc::clone(&signal_fallback_pids);
                move |candidate| match candidate.cgroup_freeze_path.as_deref() {
                    Some(_) if candidate.pid == first.pid => Ok(()),
                    Some(_) => Err(DaemonError::system("second generic freezer apply failed")),
                    None => {
                        signal_fallback_pids.borrow_mut().push(candidate.pid);
                        Ok(())
                    }
                }
            },
            |candidate| {
                assert_eq!(candidate.pid, first.pid);
                assert!(candidate.cgroup_freeze_path.is_some());
                Err(DaemonError::system("primary rollback failed"))
            },
            &[],
            0,
        )
        .expect("injected signal fallback succeeds after a partial primary transaction");

        run_control_pass(
            &mut state,
            &[config(uid, 30)],
            |_, _| Ok(processes.clone()),
            |_| panic!("foreground process must not be refrozen"),
            {
                let foreground_paths = Rc::clone(&foreground_paths);
                move |candidate| {
                    foreground_paths
                        .borrow_mut()
                        .push(candidate.cgroup_freeze_path.clone());
                    Err(DaemonError::system("generic thaw remains required"))
                }
            },
            &[uid],
            1,
        )
        .expect("foreground generic thaw failure is logged without aborting the pass");

        assert_eq!(&*signal_fallback_pids.borrow(), &[123, 456]);
        assert_eq!(
            &*foreground_paths.borrow(),
            &[
                Some("/sys/fs/cgroup/uid_10123/pid_123/cgroup.freeze".to_owned()),
                Some("/sys/fs/cgroup/uid_10123/pid_456/cgroup.freeze".to_owned()),
            ]
        );
        assert!(state.frozen_apps.contains(&identity));
        assert_eq!(
            state.frozen_ownership(&identity),
            FrozenOwnership::ResidualUnknown
        );
    }

    #[test]
    fn abnormal_audit_refreeze_retains_conservative_generic_thaw_ownership() {
        let _binder_guard = set_test_binder_available(false);
        let uid = 10_123;
        let mut runtime_process = process(uid, 123, ControlState::Running);
        runtime_process.cgroup_freeze_path =
            Some("/sys/fs/cgroup/uid_10123/pid_123/cgroup.freeze".to_owned());
        let identity = ("com.example.app".to_owned(), uid);
        let mut state = RuntimeControlState::default();
        state.track_frozen(identity.clone(), 0, FrozenOwnership::ResidualUnknown);
        let refreeze_pids = Rc::new(RefCell::new(Vec::new()));
        let foreground_paths = Rc::new(RefCell::new(Vec::new()));

        run_control_pass(
            &mut state,
            &[config(uid, 30)],
            |_, _| Ok(vec![process(uid, 123, ControlState::Frozen)]),
            |_| panic!("initial audit setup must not refreeze"),
            |_| panic!("initial audit setup must not thaw"),
            &[],
            0,
        )
        .expect("establish startup audit deadline");

        run_control_pass(
            &mut state,
            &[config(uid, 30)],
            |_, _| Ok(vec![runtime_process.clone()]),
            {
                let refreeze_pids = Rc::clone(&refreeze_pids);
                move |candidate| {
                    assert!(candidate.cgroup_freeze_path.is_none());
                    refreeze_pids.borrow_mut().push(candidate.pid);
                    Ok(())
                }
            },
            |_| panic!("audit refreeze must not thaw"),
            &[],
            60_000,
        )
        .expect("abnormal audit refreezes through signal fallback");

        run_control_pass(
            &mut state,
            &[config(uid, 30)],
            |_, _| Ok(vec![runtime_process.clone()]),
            |_| panic!("foreground process must not be refrozen"),
            {
                let foreground_paths = Rc::clone(&foreground_paths);
                move |candidate| {
                    foreground_paths
                        .borrow_mut()
                        .push(candidate.cgroup_freeze_path.clone());
                    Err(DaemonError::system("binder unfreeze failed after SIGCONT"))
                }
            },
            &[uid],
            60_001,
        )
        .expect("conservative foreground thaw failure is logged without aborting the pass");

        assert_eq!(&*refreeze_pids.borrow(), &[123]);
        assert_eq!(
            &*foreground_paths.borrow(),
            &[Some(
                "/sys/fs/cgroup/uid_10123/pid_123/cgroup.freeze".to_owned()
            )]
        );
        assert!(state.frozen_apps.contains(&identity));
        assert_eq!(
            state.frozen_ownership(&identity),
            FrozenOwnership::ResidualUnknown
        );
    }

    #[test]
    fn sigstop_break_mode_requests_network_restriction() {
        let policy = policy_from_record(&config(10_123, 21));
        let crate::domain::policy::FreezePolicy::Selected {
            allow_network_restriction,
            ..
        } = policy;

        assert!(allow_network_restriction);
    }

    #[test]
    fn mode_change_thaws_a_previously_frozen_uid_before_other_control_records_run() {
        let frozen_uid = 10_001;
        let active_uid = 10_002;
        let mut state = RuntimeControlState::default();
        let frozen_process = process(frozen_uid, 101, ControlState::Running);

        run_control_pass(
            &mut state,
            &[config(frozen_uid, 30)],
            |_, uid| {
                assert_eq!(uid, frozen_uid);
                Ok(vec![frozen_process.clone()])
            },
            |_| Ok(()),
            |_| Ok(()),
            &[],
            0,
        )
        .expect("initial freeze");

        let unfreezes = RefCell::new(Vec::new());
        run_control_pass(
            &mut state,
            &[config(frozen_uid, 40), config(active_uid, 30)],
            |_, uid| {
                if uid == frozen_uid {
                    return Ok(vec![frozen_process.clone()]);
                }
                assert_eq!(
                    uid, active_uid,
                    "the whitelist uid must not be controlled again"
                );
                Ok(Vec::new())
            },
            |_| Ok(()),
            |candidate| {
                unfreezes.borrow_mut().push(candidate.pid);
                Ok(())
            },
            &[],
            1_000,
        )
        .expect("mode-change cleanup");

        assert_eq!(&*unfreezes.borrow(), &[101]);
    }

    #[test]
    fn failed_new_pid_rollback_keeps_the_uid_tracked_for_later_recovery() {
        let _binder_guard = set_test_binder_available(true);
        let uid = 10_123;
        let mut state = RuntimeControlState::default();
        let mut discovery_count = 0;

        run_control_pass(
            &mut state,
            &[config(uid, 30)],
            |_, _| {
                discovery_count += 1;
                let mut original = process(uid, 123, ControlState::Running);
                original.cgroup_freeze_path =
                    Some("/sys/fs/cgroup/apps/uid_10123/pid_123/cgroup.freeze".to_owned());
                if discovery_count == 1 {
                    return Ok(vec![original]);
                }
                let mut new_process = process(uid, 456, ControlState::Running);
                new_process.cgroup_freeze_path =
                    Some("/sys/fs/cgroup/apps/uid_10123/pid_456/cgroup.freeze".to_owned());
                Ok(vec![original, new_process])
            },
            |_| Ok(()),
            |_| Err(DaemonError::system("thaw failed")),
            &[],
            0,
        )
        .expect("new pid rollback is recorded");

        let rows = state.freeze_status_records(
            &[config(uid, 30)],
            &BTreeMap::from([(uid, vec![process(uid, 123, ControlState::Frozen)])]),
            &[],
            1,
        );
        assert_eq!(rows[0].state, 3, "failed thaw must remain recoverable");
    }

    #[test]
    fn periodic_wakeup_thaws_a_frozen_background_uid_after_the_configured_interval() {
        let uid = 10_123;
        let mut state = RuntimeControlState::default();
        let mut settings = vec![0; 256];
        settings[2] = 0;
        settings[3] = 1;
        let mut discovery_count = 0;
        let unfreezes = RefCell::new(0);

        run_control_pass_with_settings(
            &mut state,
            &[config(uid, 30)],
            &settings,
            |_, _| {
                discovery_count += 1;
                Ok(vec![process(
                    uid,
                    123,
                    if discovery_count == 1 {
                        ControlState::Running
                    } else {
                        ControlState::Frozen
                    },
                )])
            },
            |_| Ok(()),
            |_| {
                *unfreezes.borrow_mut() += 1;
                Ok(())
            },
            &[],
            0,
        )
        .expect("initial freeze");
        run_control_pass_with_settings(
            &mut state,
            &[config(uid, 30)],
            &settings,
            |_, _| Ok(vec![process(uid, 123, ControlState::Frozen)]),
            |_| Ok(()),
            |_| {
                *unfreezes.borrow_mut() += 1;
                Ok(())
            },
            &[],
            5 * 60_000,
        )
        .expect("periodic wakeup pass");

        assert_eq!(*unfreezes.borrow(), 1);
    }

    #[test]
    fn audit_refreeze_restarts_the_periodic_wakeup_interval() {
        let uid = 10_123;
        let mut state = RuntimeControlState::default();
        let mut settings = vec![0; 256];
        settings[3] = 1;
        let unfreezes = RefCell::new(0);

        run_control_pass_with_settings(
            &mut state,
            &[config(uid, 30)],
            &settings,
            |_, _| Ok(vec![process(uid, 123, ControlState::Running)]),
            |_| Ok(()),
            |_| Ok(()),
            &[],
            0,
        )
        .expect("initial freeze");

        run_control_pass_with_settings(
            &mut state,
            &[config(uid, 30)],
            &settings,
            |_, _| Ok(vec![process(uid, 123, ControlState::Running)]),
            |_| Ok(()),
            |_| Ok(()),
            &[],
            60_000,
        )
        .expect("audit refreeze");

        run_control_pass_with_settings(
            &mut state,
            &[config(uid, 30)],
            &settings,
            |_, _| Ok(vec![process(uid, 123, ControlState::Frozen)]),
            |_| Ok(()),
            |_| {
                *unfreezes.borrow_mut() += 1;
                Ok(())
            },
            &[],
            5 * 60_000,
        )
        .expect("periodic wakeup check after an audit refreeze");

        assert_eq!(
            *unfreezes.borrow(),
            0,
            "audit refreeze must reset the next periodic wakeup deadline"
        );
    }

    #[test]
    fn disabled_regular_refreeze_does_not_recreate_an_expired_audit_deadline() {
        let mut state = RuntimeControlState::default();
        let settings = vec![0; 256];

        assert!(!refreeze_audit_due(&mut state, &settings, 0));
        assert!(!refreeze_audit_due(&mut state, &settings, 15 * 60_000));
        assert!(!refreeze_audit_due(&mut state, &settings, 15 * 60_000 + 1));
    }

    #[test]
    fn wakeup_timeout_supports_the_two_hour_spinner_value() {
        let mut settings = vec![0; 256];
        settings[3] = 5;

        assert_eq!(manager_wakeup_interval_ms(&settings), Some(120 * 60_000));
    }

    #[test]
    fn disabled_control_thaws_every_tracked_process_and_clears_pending_state() {
        let uid = 10_123;
        let mut state = RuntimeControlState::default();
        let runtime_process = process(uid, 123, ControlState::Running);
        let mut settings = vec![0; 256];
        settings[2] = 5;

        run_control_pass_with_settings(
            &mut state,
            &[config(uid, 30)],
            &settings,
            |_, _| Ok(vec![runtime_process.clone()]),
            |_| Ok(()),
            |_| Ok(()),
            &[],
            0,
        )
        .expect("schedule pending freeze");
        assert_eq!(state.pending_freeze_uids(), vec![uid]);

        run_control_pass(
            &mut state,
            &[config(uid, 30)],
            |_, _| Ok(vec![runtime_process.clone()]),
            |_| Ok(()),
            |_| Ok(()),
            &[],
            5_000,
        )
        .expect("freeze before control is disabled");

        let thawed = RefCell::new(Vec::new());
        state
            .thaw_all_frozen(
                |_, discovered_uid| {
                    assert_eq!(discovered_uid, uid);
                    Ok(vec![runtime_process.clone()])
                },
                |candidate| {
                    thawed.borrow_mut().push(candidate.pid);
                    Ok(())
                },
                |_| Ok(true),
                6_000,
            )
            .expect("disabled control thaws tracked app");

        assert_eq!(&*thawed.borrow(), &[123]);
        assert!(state.pending_freeze_uids().is_empty());
        let rows = state.freeze_status_records(
            &[config(uid, 30)],
            &BTreeMap::from([(uid, vec![runtime_process])]),
            &[],
            6_000,
        );
        assert_eq!(rows[0].state, 0);
    }

    #[test]
    fn successful_break_mode_queues_network_restriction_only_after_freeze() {
        let uid = 10_123;
        let mut state = RuntimeControlState::default();
        let signal_path_was_selected = RefCell::new(false);

        run_control_pass(
            &mut state,
            &[config(uid, 21)],
            |_, _| Ok(vec![process(uid, 123, ControlState::Running)]),
            |candidate| {
                *signal_path_was_selected.borrow_mut() = candidate.cgroup_freeze_path.is_none();
                Ok(())
            },
            |_| Ok(()),
            &[],
            0,
        )
        .expect("SIGSTOP-break freeze succeeds");

        assert!(*signal_path_was_selected.borrow());
        assert_eq!(state.take_network_restriction_uids(), vec![uid]);
        assert!(state.take_network_restriction_uids().is_empty());
    }

    #[test]
    fn clean_direct_signal_transaction_promotes_completed_stops_best_effort() {
        let processes = vec![
            process(10_123, 123, ControlState::Running),
            process(10_123, 456, ControlState::Running),
        ];
        let stopped = RefCell::new(Vec::new());
        let promoted = RefCell::new(Vec::new());

        let outcome = apply_signal_stop_freeze_transaction(
            &processes,
            &mut |_| Ok(true),
            &mut |candidate| {
                assert!(candidate.cgroup_freeze_path.is_none());
                stopped.borrow_mut().push(candidate.pid);
                Ok(())
            },
            &mut |_| Ok(()),
            SignalStopLedgerPromotion::AfterCompletedDirectTransaction,
            &mut |candidate| {
                promoted.borrow_mut().push(candidate.pid);
                if candidate.pid == 456 {
                    Err(DaemonError::system("ledger write unavailable"))
                } else {
                    Ok(())
                }
            },
        );

        assert!(outcome.failure.is_none());
        assert_eq!(&*stopped.borrow(), &[123, 456]);
        assert_eq!(&*promoted.borrow(), &[123, 456]);
    }

    #[test]
    fn incomplete_direct_signal_transaction_does_not_promote_stops() {
        let processes = vec![
            process(10_123, 123, ControlState::Running),
            process(10_123, 456, ControlState::Running),
        ];
        let promoted = RefCell::new(Vec::new());

        let outcome = apply_signal_stop_freeze_transaction(
            &processes,
            &mut |_| Ok(true),
            &mut |candidate| {
                if candidate.pid == 456 {
                    Err(DaemonError::system("second SIGSTOP failed"))
                } else {
                    Ok(())
                }
            },
            &mut |_| Ok(()),
            SignalStopLedgerPromotion::AfterCompletedDirectTransaction,
            &mut |candidate| {
                promoted.borrow_mut().push(candidate.pid);
                Ok(())
            },
        );

        assert!(outcome.failure.is_some());
        assert!(promoted.borrow().is_empty());
    }

    #[test]
    fn generic_signal_fallback_transaction_never_promotes_stop_ledger_provenance() {
        let processes = vec![
            process(10_123, 123, ControlState::Running),
            process(10_123, 456, ControlState::Running),
        ];
        let stopped = RefCell::new(Vec::new());
        let promoted = RefCell::new(Vec::new());

        let outcome = apply_signal_stop_freeze_transaction(
            &processes,
            &mut |_| Ok(true),
            &mut |candidate| {
                assert!(candidate.cgroup_freeze_path.is_none());
                stopped.borrow_mut().push(candidate.pid);
                Ok(())
            },
            &mut |_| Ok(()),
            SignalStopLedgerPromotion::Never,
            &mut |candidate| {
                promoted.borrow_mut().push(candidate.pid);
                Ok(())
            },
        );

        assert!(outcome.failure.is_none());
        assert_eq!(&*stopped.borrow(), &[123, 456]);
        assert!(promoted.borrow().is_empty());
    }

    #[test]
    fn residual_ownership_blocks_later_direct_signal_ledger_promotion() {
        let identity = ("com.example.app".to_owned(), 10_123);
        let mut state = RuntimeControlState::default();
        state.track_frozen(identity.clone(), 0, FrozenOwnership::ResidualUnknown);
        state.mark_abnormal_thaw_for_refreeze(&identity);
        let promoted = RefCell::new(Vec::new());
        let processes = vec![process(10_123, 123, ControlState::Running)];

        let outcome = apply_signal_stop_freeze_transaction(
            &processes,
            &mut |_| Ok(true),
            &mut |_| Ok(()),
            &mut |_| Ok(()),
            signal_stop_ledger_promotion(&state, &identity, &processes, &mut |_, _| Ok(None)),
            &mut |candidate| {
                promoted.borrow_mut().push(candidate.pid);
                Ok(())
            },
        );

        assert!(outcome.failure.is_none());
        assert!(promoted.borrow().is_empty());
        assert_eq!(
            state.frozen_ownership(&identity),
            FrozenOwnership::ResidualUnknown
        );
    }

    #[test]
    fn persisted_residual_ledger_blocks_direct_signal_promotion_after_restart() {
        let identity = ("com.example.app".to_owned(), 10_123);
        let mut state = RuntimeControlState::default();
        let processes = vec![process(10_123, 123, ControlState::Running)];
        let promoted = RefCell::new(Vec::new());

        let promotion = signal_stop_ledger_promotion(
            &state,
            &identity,
            &processes,
            &mut |pid, start_time_ticks| {
                assert_eq!((pid, start_time_ticks), (123, 1));
                Ok(Some(SignalStopOwnership::ResidualUnknown))
            },
        );
        let outcome = apply_signal_stop_freeze_transaction(
            &processes,
            &mut |_| Ok(true),
            &mut |_| Ok(()),
            &mut |_| Ok(()),
            promotion,
            &mut |candidate| {
                promoted.borrow_mut().push(candidate.pid);
                Ok(())
            },
        );

        assert!(outcome.failure.is_none());
        assert!(promoted.borrow().is_empty());

        state.track_frozen(identity, 0, promotion.frozen_ownership());
        let mut rediscovered_process = processes[0].clone();
        rediscovered_process.cgroup_freeze_path =
            Some("/sys/fs/cgroup/uid_10123/pid_123/cgroup.freeze".to_owned());
        let thaw_paths = RefCell::new(Vec::new());
        run_control_pass(
            &mut state,
            &[config(10_123, 20)],
            |_, _| Ok(vec![rediscovered_process.clone()]),
            |_| panic!("foreground process must not be frozen"),
            |candidate| {
                thaw_paths
                    .borrow_mut()
                    .push(candidate.cgroup_freeze_path.clone());
                Ok(())
            },
            &[10_123],
            1,
        )
        .expect("residual foreground thaw");

        assert_eq!(
            &*thaw_paths.borrow(),
            &[Some(
                "/sys/fs/cgroup/uid_10123/pid_123/cgroup.freeze".to_owned()
            )]
        );
    }

    #[test]
    fn global_sigstop_overrides_freezer_mode_selection() {
        let mut settings = vec![0; 256];
        settings[5] = 2;

        assert_eq!(
            requested_control_backend(&config(10_123, 30), &settings),
            RequestedControlBackend::Signal
        );
    }

    #[test]
    fn terminate_mode_uses_the_injected_sigkill_backend_after_identity_validation() {
        let uid = 10_123;
        let mut state = RuntimeControlState::default();
        let killed = Rc::new(RefCell::new(Vec::new()));
        let killed_by_backend = Rc::clone(&killed);

        run_control_pass_with_sampling_and_terminate(
            &mut state,
            &[config(uid, 10)],
            &[],
            |_, _| Ok(vec![process(uid, 123, ControlState::Running)]),
            |_| Ok(()),
            |_| Ok(()),
            |_| Ok(true),
            |_, _| Ok(None),
            move |candidate| {
                killed_by_backend.borrow_mut().push(candidate.pid);
                Ok(())
            },
            &[],
            0,
        )
        .expect("terminate control pass");

        assert_eq!(&*killed.borrow(), &[123]);
        assert!(state.operation_log.to_json().contains("signal.kill"));
    }

    #[test]
    fn restart_recovery_refuses_sigcont_after_identity_drift() {
        let runtime_process = process(10_123, 123, ControlState::Frozen);
        let signals = RefCell::new(Vec::new());
        let outcome = recover_process_after_restart(
            &runtime_process,
            Some(SignalStopOwnership::SignalOnly),
            None,
            &mut |_| Ok(false),
            &mut |_| -> Result<(), DaemonError> { Ok(()) },
            &mut |_| -> Result<(), DaemonError> { Ok(()) },
            &mut |candidate| {
                signals.borrow_mut().push(candidate.pid);
                Ok(())
            },
        );

        assert!(signals.borrow().is_empty());
        assert!(outcome
            .failures
            .iter()
            .any(|failure| failure.contains("immediately before SIGCONT")));
    }

    #[test]
    fn restart_signal_only_stop_with_thawed_cgroup_resumes_without_generic_cleanup() {
        let mut runtime_process = process(10_123, 123, ControlState::Frozen);
        runtime_process.cgroup_freeze_path =
            Some("/sys/fs/cgroup/uid_10123/pid_123/cgroup.freeze".to_owned());
        let calls = RefCell::new(Vec::new());
        let outcome = recover_process_after_restart(
            &runtime_process,
            Some(SignalStopOwnership::SignalOnly),
            Some(FreezeState::Thawed),
            &mut |_| Ok(true),
            &mut |path| {
                calls.borrow_mut().push(format!("cgroup:{path}"));
                Ok(())
            },
            &mut |candidate| {
                calls.borrow_mut().push(format!("binder:{}", candidate.pid));
                Ok(())
            },
            &mut |candidate| {
                calls
                    .borrow_mut()
                    .push(format!("sigcont:{}", candidate.pid));
                Ok(())
            },
        );

        assert!(!outcome.cgroup_thawed);
        assert!(outcome.signal_resumed);
        assert!(outcome.failures.is_empty());
        assert_eq!(&*calls.borrow(), &["sigcont:123".to_owned()]);
    }

    #[test]
    fn restart_residual_unknown_stop_with_thawed_cgroup_keeps_generic_cleanup() {
        let mut runtime_process = process(10_123, 123, ControlState::Frozen);
        runtime_process.cgroup_freeze_path =
            Some("/sys/fs/cgroup/uid_10123/pid_123/cgroup.freeze".to_owned());
        let calls = RefCell::new(Vec::new());
        let outcome = recover_process_after_restart(
            &runtime_process,
            Some(SignalStopOwnership::ResidualUnknown),
            Some(FreezeState::Thawed),
            &mut |_| Ok(true),
            &mut |path| {
                calls.borrow_mut().push(format!("cgroup:{path}"));
                Ok(())
            },
            &mut |candidate| {
                calls.borrow_mut().push(format!("binder:{}", candidate.pid));
                Ok(())
            },
            &mut |candidate| {
                calls
                    .borrow_mut()
                    .push(format!("sigcont:{}", candidate.pid));
                Ok(())
            },
        );

        assert!(outcome.cgroup_thawed);
        assert!(outcome.signal_resumed);
        assert!(outcome.failures.is_empty());
        assert_eq!(
            &*calls.borrow(),
            &[
                "cgroup:/sys/fs/cgroup/uid_10123/pid_123/cgroup.freeze".to_owned(),
                "binder:123".to_owned(),
                "sigcont:123".to_owned(),
            ]
        );
    }

    #[test]
    fn restart_recovery_thaws_cgroup_and_binder_without_waiting_for_sigstop_state() {
        let mut runtime_process = process(10_123, 123, ControlState::Frozen);
        runtime_process.cgroup_freeze_path =
            Some("/sys/fs/cgroup/uid_10123/pid_123/cgroup.freeze".to_owned());
        let calls = RefCell::new(Vec::new());
        let outcome = recover_process_after_restart(
            &runtime_process,
            None,
            Some(FreezeState::Thawed),
            &mut |_| Ok(true),
            &mut |path| {
                calls.borrow_mut().push(format!("cgroup:{path}"));
                Ok(())
            },
            &mut |candidate| {
                calls.borrow_mut().push(format!("binder:{}", candidate.pid));
                Ok(())
            },
            &mut |candidate| {
                calls
                    .borrow_mut()
                    .push(format!("sigcont:{}", candidate.pid));
                Ok(())
            },
        );

        assert!(outcome.cgroup_thawed);
        assert!(!outcome.signal_resumed);
        assert_eq!(
            &*calls.borrow(),
            &[
                "cgroup:/sys/fs/cgroup/uid_10123/pid_123/cgroup.freeze".to_owned(),
                "binder:123".to_owned(),
            ]
        );
    }
}
