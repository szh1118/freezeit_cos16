use std::{collections::BTreeMap, fs};

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
        operation_log::OperationLog,
        package_inventory::{parse_cmd_package_list, reconcile_uid, PackageRecord},
    },
    config::{
        loader::{load_policy_files, load_policy_files_recovering, DaemonPaths, LoadedPolicyFiles},
        migration::parse_legacy_policy_line,
    },
    domain::{
        capability::ControlCapability,
        operation::{ControlAction, ControlOperation, OperationResult},
        policy::{FallbackAction, ForegroundStrategy, FreezeMode, FreezePolicy, ManagedApp},
        runtime::RuntimeProcess,
    },
    protocol::{
        manager_v1::{
            encode_app_config, encode_xposed_config_payload, handle_read_only_command,
            normalize_settings, ManagerAppConfigRecord, ManagerCommand, ReadOnlyState,
        },
        manager_v2::{
            capability_report_json, compatibility_report_json, health_report_json,
            operation_log_json, self_check_json,
        },
        xposed::{classify_bridge_error, classify_hook_health_payload},
    },
    sys::{socket, xposed_bridge},
};

pub fn run() -> Result<(), DaemonError> {
    run_with_paths(&DaemonPaths::from_module_dir(
        crate::config::loader::DEFAULT_MODULE_DIR,
    ))
}

pub fn run_with_paths(paths: &DaemonPaths) -> Result<(), DaemonError> {
    let mut state = startup_read_only_state_from_paths(paths);
    if let Err(error) = sync_loaded_config_to_hook(&mut state, xposed_bridge::set_config) {
        append_log_once(
            &mut state.log,
            &format!("hook config sync failed: {error}\n"),
        );
    }
    socket::run_manager_server_forever(state)
}

pub fn startup_read_only_state() -> ReadOnlyState {
    startup_read_only_state_from_paths(&DaemonPaths::from_module_dir(
        crate::config::loader::DEFAULT_MODULE_DIR,
    ))
}

pub fn startup_read_only_state_from_paths(paths: &DaemonPaths) -> ReadOnlyState {
    let mut state = ReadOnlyState::default();
    state.settings_path = Some(paths.settings_db.clone());
    state.app_config_path = Some(paths.app_config.clone());
    state.app_label_path = Some(paths.app_label.clone());
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
        state.log = sanitize_startup_log(&log);
        if !state.log.ends_with('\n') {
            state.log.push('\n');
        }
    }
    let policy = load_policy_files_recovering(paths);
    let policy_ready = policy.is_available();
    state.settings = normalize_settings(policy.settings);
    let package_records = load_package_records();
    state.app_config =
        load_manager_app_config_records(policy.app_config.as_deref(), &package_records);
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
    let capabilities =
        SystemAwareCgroupBinderBackend::new(BackendEnvironment::detect()).discover_capabilities();
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
        append_log_once(
            &mut state.log,
            &format!(
                "hook config synced: managed_apps={} settings={}\n",
                state.app_config.len(),
                state.settings.len()
            ),
        );
    } else {
        append_log_once(&mut state.log, "hook config sync rejected\n");
    }
    Ok(synced)
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
    state.log.push_str(&format!(
        "daemon active: apps={} settings={} android={} kernel={}\n",
        state.app_config.len(),
        state.settings.len(),
        state.android_version,
        state.kernel_version
    ));
}

fn append_log_once(log: &mut String, line: &str) {
    let trimmed = line.trim_end();
    if !log.lines().any(|existing| existing == trimmed) {
        log.push_str(line);
        if !line.ends_with('\n') {
            log.push('\n');
        }
    }
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
            let uid = parse_legacy_uid_token(&record.package_or_uid)
                .or_else(|| package_uids.get(record.package_or_uid.as_str()).copied())?;
            Some(ManagerAppConfigRecord {
                uid,
                mode: record.mode,
                permissive: record.permissive,
            })
        })
        .collect()
}

fn parse_legacy_uid_token(token: &str) -> Option<u32> {
    if let Ok(uid) = token.parse::<u32>() {
        return Some(uid);
    }
    token
        .split_once("uid")
        .and_then(|(_, uid)| uid.parse::<u32>().ok())
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
    state.self_check_json = self_check_json(&health, capabilities);
    state.compatibility_report_json = compatibility_report_json(runtime, capabilities);
    state.control_allowed = runtime.allows_control(capabilities);
}

pub fn refresh_runtime_diagnostics(state: &mut ReadOnlyState) {
    let capabilities =
        SystemAwareCgroupBinderBackend::new(BackendEnvironment::detect()).discover_capabilities();
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
    let mut state = ReadOnlyState::default();
    state.health_report_json = health_report_json(&diagnostics.health);
    state.capability_report_json = capability_report_json(&diagnostics.capabilities);
    state.compatibility_report_json = compatibility_report_json(
        &RuntimeEnvironment::new("unknown", "unknown", 0, "", "unknown", false, false, false),
        &diagnostics.capabilities,
    );
    state.operation_log_json = operation_log_json(&diagnostics.operation_log);
    state.operation_log_text = diagnostics.operation_log.to_legacy_text();
    state.self_check_json = self_check_json(&diagnostics.health, &diagnostics.capabilities);
    state
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeControlState {
    pub operation_log: OperationLog,
    frozen_apps: std::collections::BTreeSet<(String, u32)>,
    pending_freezes: BTreeMap<(String, u32), u128>,
    next_operation_id: u64,
    download_deferral: DownloadDeferral,
    audit_started_at_ms: Option<u128>,
    next_refreeze_audit_at_ms: Option<u128>,
}

impl Default for RuntimeControlState {
    fn default() -> Self {
        Self {
            operation_log: OperationLog::new(128),
            frozen_apps: std::collections::BTreeSet::new(),
            pending_freezes: BTreeMap::new(),
            next_operation_id: 1,
            download_deferral: DownloadDeferral::default(),
            audit_started_at_ms: None,
            next_refreeze_audit_at_ms: None,
        }
    }
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
    let audit_due = refreeze_audit_due(state, settings, timestamp_ms);
    for record in app_config {
        if !is_control_policy_mode(record.mode) {
            continue;
        }
        let fallback_package_name = format!("uid{}", record.uid);
        let processes = discover_processes(&fallback_package_name, record.uid)?;
        if processes.is_empty() {
            continue;
        }
        let package_name = processes
            .first()
            .map(|process| process.package_name.clone())
            .unwrap_or_else(|| fallback_package_name.clone());
        let app = managed_app_from_record(&package_name, record.uid);
        let policy = policy_from_record(record);
        let identity = (package_name.clone(), record.uid);

        if foreground_uids.contains(&record.uid) {
            state.pending_freezes.remove(&identity);
            if state.frozen_apps.contains(&identity) {
                let mut failure = None;
                if processes
                    .iter()
                    .any(|process| process.package_name != package_name)
                {
                    failure = Some("shared uid contains multiple package identities".to_owned());
                }
                for process in &processes {
                    if failure.is_none() && !validate_process(process)? {
                        failure = Some(format!(
                            "identity validation failed before unfreeze for pid {}",
                            process.pid
                        ));
                    }
                }
                if failure.is_none() {
                    for process in &processes {
                        if !validate_process(process)? {
                            failure = Some(format!(
                                "identity validation failed before unfreeze signal for pid {}",
                                process.pid
                            ));
                            break;
                        }
                        if let Err(error) = unfreeze_process(process) {
                            failure =
                                Some(format!("unfreeze failed for pid {}: {error}", process.pid));
                            break;
                        }
                    }
                }
                if let Some(reason) = failure {
                    let mut operation = ControlOperation {
                        operation_id: 0,
                        timestamp_ms: 0,
                        package_name: package_name.clone(),
                        uid: record.uid,
                        pid_list: processes.iter().map(|process| process.pid).collect(),
                        action: ControlAction::Unfreeze,
                        backend: backend_name(&processes).to_owned(),
                        reason,
                        result: OperationResult::Failed,
                        details: operation_details(&processes),
                    };
                    stamp_operation(&mut operation, state, timestamp_ms);
                    state.operation_log.push(operation);
                    continue;
                }
                state.frozen_apps.remove(&identity);
                let mut operation =
                    SystemAwareCgroupBinderBackend::new(backend_environment(&processes))
                        .unfreeze_operation(&app, &processes, "foreground uid active");
                operation.backend = backend_name(&processes).to_owned();
                stamp_operation(&mut operation, state, timestamp_ms);
                state.operation_log.push(operation);
            }
            continue;
        }

        let mut abnormal_thaw = false;
        if state.frozen_apps.contains(&identity) {
            if !audit_due
                || processes.iter().all(|process| {
                    process.control_state == crate::domain::runtime::ControlState::Frozen
                })
            {
                continue;
            }
            state.frozen_apps.remove(&identity);
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
                state
                    .pending_freezes
                    .insert(identity.clone(), timestamp_ms + u128::from(retry_ms));
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
                continue;
            }
        }

        let mut pending_processes = processes.clone();
        for process in &mut pending_processes {
            process.control_state = crate::domain::runtime::ControlState::PendingFreeze;
        }
        let backend = SystemAwareCgroupBinderBackend::new(backend_environment(&pending_processes));
        let decision = backend.can_freeze(&app, &policy, &pending_processes);
        let (action, result) = match decision.action {
            DecisionAction::Freeze => {
                let freeze_outcome = apply_freeze_transaction(
                    &pending_processes,
                    &mut validate_process,
                    &mut freeze_process,
                    &mut unfreeze_process,
                );
                if let Some(failure) = freeze_outcome.failure {
                    if freeze_outcome.residual_possible {
                        state.frozen_apps.insert(identity.clone());
                    }
                    let error = DaemonError::system(failure.clone());
                    let fallback = backend.fallback_after_freeze_apply_error(&policy, &error);
                    let (action, result, fallback_backend, fallback_details) = match fallback.action
                    {
                        DecisionAction::Signal => {
                            let outcome = apply_freeze_transaction(
                                &pending_processes,
                                &mut validate_process,
                                &mut |process| {
                                    crate::sys::signal::send_signal(
                                        process.pid,
                                        crate::sys::signal::SignalAction::Stop,
                                    )
                                },
                                &mut |process| {
                                    crate::sys::signal::send_signal(
                                        process.pid,
                                        crate::sys::signal::SignalAction::Continue,
                                    )
                                },
                            );
                            if outcome.failure.is_none() {
                                state.frozen_apps.insert(identity.clone());
                                (
                                    ControlAction::Freeze,
                                    OperationResult::Success,
                                    "signal.stop",
                                    format!("signal_applied={}", outcome.applied),
                                )
                            } else {
                                if outcome.rollback_failures > 0 {
                                    state.frozen_apps.insert(identity.clone());
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
                    for process in &post_freeze_processes {
                        let _ = unfreeze_process(process);
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
                            "{} new_pids={new_pids:?}",
                            operation_details(&post_freeze_processes)
                        ),
                    };
                    stamp_operation(&mut operation, state, timestamp_ms);
                    state.operation_log.push(operation);
                    continue;
                }
                state.frozen_apps.insert(identity);
                state
                    .pending_freezes
                    .remove(&(package_name.clone(), record.uid));
                (ControlAction::Freeze, OperationResult::Success)
            }
            DecisionAction::Postpone => (ControlAction::Postpone, OperationResult::Postponed),
            DecisionAction::AlternateFreezer => (ControlAction::Fallback, OperationResult::Failed),
            DecisionAction::Signal => {
                let outcome = apply_freeze_transaction(
                    &pending_processes,
                    &mut validate_process,
                    &mut freeze_process,
                    &mut unfreeze_process,
                );
                if let Some(failure) = outcome.failure {
                    if outcome.rollback_failures > 0 {
                        state.frozen_apps.insert(identity.clone());
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
                        backend: backend_name(&pending_processes).to_owned(),
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
                    continue;
                }
                state.frozen_apps.insert(identity);
                state
                    .pending_freezes
                    .remove(&(package_name.clone(), record.uid));
                (ControlAction::Freeze, OperationResult::Success)
            }
            DecisionAction::Terminate => (ControlAction::Terminate, OperationResult::Failed),
            DecisionAction::Skip => (ControlAction::Skip, OperationResult::Skipped),
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
            backend: backend_name(&pending_processes).to_owned(),
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
    let next = state
        .next_refreeze_audit_at_ms
        .get_or_insert(started_at + 60_000);
    if timestamp_ms < *next {
        return false;
    }
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
    state.next_refreeze_audit_at_ms = interval_ms.map(|interval| timestamp_ms + interval);
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
            failure = Some(format!("signal failed for pid {}: {error}", process.pid));
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
    BackendEnvironment {
        cgroup_available: !processes.is_empty()
            && processes
                .iter()
                .all(|process| process.cgroup_freeze_path.is_some()),
        binder_available: crate::sys::binder::detect_binder_freezer_capability().status
            == crate::domain::capability::CapabilityStatus::Available,
        network_available: true,
        wakelock_available: true,
        screen_state_available: true,
        hook_fresh: true,
    }
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
    ManagedApp {
        package_name: package_name.to_owned(),
        user_id: 0,
        uid,
        label: package_name.to_owned(),
        is_system_app: false,
        protected_reason: None,
        policy_id: "manager-v1".to_owned(),
        last_seen_baseline: "runtime".to_owned(),
    }
}

fn policy_from_record(record: &ManagerAppConfigRecord) -> FreezePolicy {
    let mode = match record.mode {
        10 => FreezeMode::Terminate,
        20 | 21 | 30 | 31 => FreezeMode::Freeze,
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
        allow_network_restriction: record.mode == 31,
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
