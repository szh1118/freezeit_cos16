use std::{
    collections::BTreeSet,
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    app::{
        controller::{
            is_control_policy_mode, reconcile_live_process_control_states,
            refresh_runtime_diagnostics, run_control_pass_with_sampling, RuntimeControlState,
        },
        error::DaemonError,
        logging::LogRecord,
    },
    protocol::{
        manager_v1::{
            encode_app_config, encode_frame, encode_xposed_config_payload, handle_manager_command,
            parse_frame, ManagerCommand, ManagerFrame, ReadOnlyState, HEADER_LEN,
            MANAGER_LISTEN_HOST, MANAGER_LISTEN_PORT, MAX_PAYLOAD_LEN,
        },
        xposed::{classify_bridge_error, classify_hook_health_payload, XposedCommand},
    },
    sys::{binder, cgroup, procfs, signal, xposed_bridge},
};

/// 一次管理器请求的读取/写入超时。设置后慢客户端不能永久占住 state 锁阻塞控制循环。
const MANAGER_STREAM_TIMEOUT: Duration = Duration::from_secs(5);

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

pub fn bind_manager_listener() -> Result<TcpListener, DaemonError> {
    Ok(TcpListener::bind((
        MANAGER_LISTEN_HOST,
        MANAGER_LISTEN_PORT,
    ))?)
}

fn deadline_exceeded() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "manager frame deadline exceeded")
}

/// `TcpStream` timeouts apply to each individual read. Recompute the
/// remaining duration before every read to enforce one deadline for the whole
/// frame, including a client that sends a byte just before every timeout.
fn read_exact_with_deadline<R: Read>(
    reader: &mut R,
    buffer: &mut [u8],
    deadline: Instant,
    mut set_read_timeout: impl FnMut(&mut R, Option<Duration>) -> io::Result<()>,
) -> io::Result<()> {
    let mut offset = 0;
    while offset < buffer.len() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(deadline_exceeded)?;
        if remaining.is_zero() {
            return Err(deadline_exceeded());
        }
        set_read_timeout(reader, Some(remaining))?;
        match reader.read(&mut buffer[offset..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "manager frame ended before all bytes arrived",
                ));
            }
            Ok(read) => {
                offset += read;
                if Instant::now() >= deadline {
                    return Err(deadline_exceeded());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(deadline_exceeded());
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn write_all_with_deadline<W: Write>(
    writer: &mut W,
    bytes: &[u8],
    deadline: Instant,
    mut set_write_timeout: impl FnMut(&mut W, Option<Duration>) -> io::Result<()>,
) -> io::Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(deadline_exceeded)?;
        if remaining.is_zero() {
            return Err(deadline_exceeded());
        }
        set_write_timeout(writer, Some(remaining))?;
        match writer.write(&bytes[offset..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "manager response write made no progress",
                ));
            }
            Ok(written) => {
                offset += written;
                if Instant::now() >= deadline {
                    return Err(deadline_exceeded());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(deadline_exceeded());
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn read_manager_request(stream: &mut TcpStream) -> Result<ManagerFrame, DaemonError> {
    let deadline = Instant::now() + MANAGER_STREAM_TIMEOUT;
    let mut header = [0_u8; HEADER_LEN];
    read_exact_with_deadline(stream, &mut header, deadline, |stream, timeout| {
        stream.set_read_timeout(timeout)
    })?;
    let payload_len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if payload_len > MAX_PAYLOAD_LEN {
        return Err(DaemonError::protocol(format!(
            "manager frame payload too large: {payload_len} > {MAX_PAYLOAD_LEN}"
        )));
    }
    let mut bytes = header.to_vec();
    bytes.resize(HEADER_LEN + payload_len, 0);
    read_exact_with_deadline(
        stream,
        &mut bytes[HEADER_LEN..],
        deadline,
        |stream, timeout| stream.set_read_timeout(timeout),
    )?;
    parse_frame(&bytes)
}

fn write_manager_response(stream: &mut TcpStream, response: &[u8]) -> Result<(), DaemonError> {
    let deadline = Instant::now() + MANAGER_STREAM_TIMEOUT;
    write_all_with_deadline(stream, response, deadline, |stream, timeout| {
        stream.set_write_timeout(timeout)
    })?;
    Ok(())
}

fn handle_manager_request(
    request: &ManagerFrame,
    state: &mut ReadOnlyState,
) -> Result<Vec<u8>, DaemonError> {
    // Callers that own shared state run policy updates against a detached
    // working copy, so this bridge callback cannot retain the global state
    // mutex while Xposed performs its socket round-trip.
    handle_manager_command(request, state, xposed_bridge::set_config)
}

/// Compatibility entry point for the controller's one-shot helper. The
/// long-running server reads first through `serve_manager_connection`.
pub fn handle_single_manager_stream(
    mut stream: TcpStream,
    state: &mut ReadOnlyState,
) -> Result<(), DaemonError> {
    let request = read_manager_request(&mut stream)?;
    let payload = handle_manager_request(&request, state)?;
    let response = encode_frame(request.command, &payload)?;
    write_manager_response(&mut stream, &response)
}

fn serve_manager_connection(
    mut stream: TcpStream,
    state: &Arc<Mutex<ReadOnlyState>>,
    policy_action_gate: &Arc<Mutex<()>>,
) -> Result<(), DaemonError> {
    // Read untrusted TCP bytes before either shared mutex. A policy mutation
    // then holds the action gate through its Xposed work and state commit so a
    // concurrently scheduled control pass cannot use an older policy snapshot.
    let request = read_manager_request(&mut stream)?;
    let response = if is_policy_mutation_command(request.command) {
        with_policy_action_gate(policy_action_gate, || {
            with_manager_state_after_request_read(
                state,
                || Ok(request),
                |request, state| {
                    let payload = handle_manager_request(request, state)?;
                    encode_frame(request.command, &payload)
                },
            )
        })?
    } else {
        with_manager_state_after_request_read(
            state,
            || Ok(request),
            |request, state| {
                let payload = handle_manager_request(request, state)?;
                encode_frame(request.command, &payload)
            },
        )?
    };
    write_manager_response(&mut stream, &response)
}

fn is_policy_mutation_command(command: ManagerCommand) -> bool {
    matches!(
        command,
        ManagerCommand::SetAppCfg | ManagerCommand::SetSettingsVar
    )
}

fn with_policy_action_gate<T>(
    policy_action_gate: &Arc<Mutex<()>>,
    action: impl FnOnce() -> Result<T, DaemonError>,
) -> Result<T, DaemonError> {
    let _gate = policy_action_gate
        .lock()
        .map_err(|_| DaemonError::system("policy action mutex poisoned"))?;
    action()
}

fn with_manager_state_after_request_read<T>(
    state: &Arc<Mutex<ReadOnlyState>>,
    read_request: impl FnOnce() -> Result<ManagerFrame, DaemonError>,
    handle_request: impl FnOnce(&ManagerFrame, &mut ReadOnlyState) -> Result<T, DaemonError>,
) -> Result<T, DaemonError> {
    // Do not hold state while reading attacker-controlled bytes.
    let request = read_request()?;
    if is_policy_mutation_command(request.command) {
        // SetAppCfg can synchronously call the Xposed bridge and SetSettingsVar
        // can perform durable storage I/O. Work on a detached copy while their
        // policy-action gate is held, then persist every in-memory effect even
        // if the handler returns an error (for example, rollback diagnostics or
        // an invalidated hook sync claim).
        let mut working_state = state
            .lock()
            .map_err(|_| DaemonError::system("manager state mutex poisoned"))?
            .clone();
        let result = handle_request(&request, &mut working_state);
        let mut state = state
            .lock()
            .map_err(|_| DaemonError::system("manager state mutex poisoned"))?;
        *state = working_state;
        result
    } else {
        let mut state = state
            .lock()
            .map_err(|_| DaemonError::system("manager state mutex poisoned"))?;
        handle_request(&request, &mut state)
    }
}

fn run_live_control_pass(
    state: &mut ReadOnlyState,
    control_state: &mut RuntimeControlState,
) -> Result<(), DaemonError> {
    if !should_run_control_pass(state) {
        return thaw_disabled_control_state(
            state,
            control_state,
            |_, uid| {
                let mut uids = BTreeSet::new();
                uids.insert(uid);
                let mut by_uid = procfs::discover_managed_uid_processes(procfs::PROC_ROOT, &uids)?;
                Ok(by_uid.remove(&uid).unwrap_or_default())
            },
            unfreeze_process,
            |process| procfs::recheck_process_identity(procfs::PROC_ROOT, process),
            now_ms(),
        );
    }

    let control_uids = state
        .app_config
        .iter()
        .filter(|record| is_control_policy_mode(record.mode))
        .map(|record| record.uid)
        .collect::<BTreeSet<_>>();
    let mut processes_by_uid =
        procfs::discover_managed_uid_processes(procfs::PROC_ROOT, &control_uids)?;
    for processes in processes_by_uid.values_mut() {
        reconcile_live_process_control_states(
            processes,
            |path| cgroup::read_freeze_state(path),
            |pid| procfs::read_proc_state_char(procfs::PROC_ROOT, pid),
        );
    }
    let foreground_uids =
        require_foreground_uids_for_control(xposed_bridge::query_foreground_uids())?;
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);

    let has_download_candidate = processes_by_uid
        .values()
        .flatten()
        .any(|process| crate::app::download_deferral::is_candidate_package(&process.package_name));
    let uid_rx_bytes = if has_download_candidate {
        Some(crate::app::download_deferral::sample_uid_rx_bytes_map())
    } else {
        None
    };

    run_control_pass_with_sampling(
        control_state,
        &state.app_config,
        &state.settings,
        |_package_name, uid| Ok(processes_by_uid.get(&uid).cloned().unwrap_or_default()),
        freeze_process,
        unfreeze_process,
        |process| procfs::recheck_process_identity(procfs::PROC_ROOT, process),
        |uid, _| match &uid_rx_bytes {
            Some(Ok(values)) => Ok(values.get(&uid).copied()),
            Some(Err(error)) => Err(DaemonError::system(error.to_string())),
            None => Ok(None),
        },
        &foreground_uids,
        timestamp_ms,
    )?;
    state.freeze_status = control_state.freeze_status_records(
        &state.app_config,
        &processes_by_uid,
        &foreground_uids,
        timestamp_ms,
    );
    Ok(())
}

fn thaw_disabled_control_state(
    state: &mut ReadOnlyState,
    control_state: &mut RuntimeControlState,
    discover_processes: impl FnMut(
        &str,
        u32,
    )
        -> Result<Vec<crate::domain::runtime::RuntimeProcess>, DaemonError>,
    unfreeze: impl FnMut(&crate::domain::runtime::RuntimeProcess) -> Result<(), DaemonError>,
    validate: impl FnMut(&crate::domain::runtime::RuntimeProcess) -> Result<bool, DaemonError>,
    timestamp_ms: u128,
) -> Result<(), DaemonError> {
    // Clear the UI snapshot only after attempting every tracked thaw. The
    // controller retains identities whose thaw failed so the next pass retries.
    let result =
        control_state.thaw_all_frozen(discover_processes, unfreeze, validate, timestamp_ms);
    state.freeze_status.clear();
    result
}

struct HookRuntimeSyncOutcome {
    pending_error: Option<String>,
    failed_network_uids: Vec<u32>,
}

fn encode_uid_payload(uids: &[u32]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(uids.len() * std::mem::size_of::<u32>());
    for uid in uids {
        payload.extend_from_slice(&uid.to_le_bytes());
    }
    payload
}

fn require_xposed_success(command: XposedCommand, response: &[u8]) -> Result<(), DaemonError> {
    if response.len() < std::mem::size_of::<i32>() {
        return Err(DaemonError::protocol(format!(
            "xposed {command:?} response header is incomplete"
        )));
    }
    if i32::from_le_bytes([response[0], response[1], response[2], response[3]]) != 2 {
        return Err(DaemonError::system(format!(
            "xposed {command:?} rejected request"
        )));
    }
    Ok(())
}

fn sync_hook_runtime_state(
    pending_uids: &[u32],
    network_uids: &[u32],
    mut request: impl FnMut(XposedCommand, &[u8]) -> Result<Vec<u8>, DaemonError>,
) -> HookRuntimeSyncOutcome {
    // Empty is meaningful: it clears a stale pending list held by the hook.
    let pending_payload = encode_uid_payload(pending_uids);
    let pending_error = request(XposedCommand::UpdatePending, &pending_payload)
        .and_then(|response| require_xposed_success(XposedCommand::UpdatePending, &response))
        .err()
        .map(|error| error.to_string());

    let mut failed_network_uids = Vec::new();
    for uid in network_uids {
        let payload = uid.to_le_bytes();
        let result = request(XposedCommand::BreakNetwork, &payload)
            .and_then(|response| require_xposed_success(XposedCommand::BreakNetwork, &response));
        if result.is_err() {
            failed_network_uids.push(*uid);
        }
    }

    HookRuntimeSyncOutcome {
        pending_error,
        failed_network_uids,
    }
}

pub fn should_run_control_pass(state: &ReadOnlyState) -> bool {
    state.control_allowed
        && state.hook_health == "active"
        && state.hook_config_synced
        && state
            .app_config
            .iter()
            .any(|record| is_control_policy_mode(record.mode))
}

pub fn require_foreground_uids_for_control(
    foreground_uids: Result<Vec<u32>, DaemonError>,
) -> Result<Vec<u32>, DaemonError> {
    foreground_uids
}

fn freeze_process(process: &crate::domain::runtime::RuntimeProcess) -> Result<(), DaemonError> {
    if let Some(path) = &process.cgroup_freeze_path {
        let binder_pid = u32::try_from(process.pid)
            .map_err(|_| DaemonError::system(format!("invalid binder pid {}", process.pid)))?;
        let binder_path = binder::discover_binder_device()
            .ok_or_else(|| DaemonError::system("binder device disappeared before freeze"))?;
        binder::set_binder_freeze(
            binder_path,
            binder_pid,
            binder::BinderFreezeRequest::Freeze,
            0,
        )?;
        if let Err(error) = cgroup::write_freeze_state(path, cgroup::FreezeState::Frozen) {
            if let Err(cleanup_error) = binder::set_binder_freeze(
                binder_path,
                binder_pid,
                binder::BinderFreezeRequest::Unfreeze,
                0,
            ) {
                return Err(DaemonError::system(format!(
                    "cgroup freeze failed: {error}; binder rollback failed: {cleanup_error}"
                )));
            }
            return Err(error);
        }
        Ok(())
    } else {
        signal::send_signal(process.pid, signal::SignalAction::Stop)
    }
}

fn unfreeze_process(process: &crate::domain::runtime::RuntimeProcess) -> Result<(), DaemonError> {
    run_unfreeze_sequence(
        process.cgroup_freeze_path.as_deref(),
        |path| cgroup::write_freeze_state(path, cgroup::FreezeState::Thawed),
        || {
            let binder_pid = u32::try_from(process.pid)
                .map_err(|_| DaemonError::system(format!("invalid binder pid {}", process.pid)))?;
            let binder_path = binder::discover_binder_device()
                .ok_or_else(|| DaemonError::system("binder device disappeared before unfreeze"))?;
            Ok(binder::set_binder_freeze(
                binder_path,
                binder_pid,
                binder::BinderFreezeRequest::Unfreeze,
                0,
            )?)
        },
        || signal::send_signal(process.pid, signal::SignalAction::Continue),
    )
}

fn run_unfreeze_sequence(
    cgroup_path: Option<&str>,
    thaw_cgroup: impl FnOnce(&str) -> Result<(), DaemonError>,
    unfreeze_binder: impl FnOnce() -> Result<(), DaemonError>,
    resume_signal: impl FnOnce() -> Result<(), DaemonError>,
) -> Result<(), DaemonError> {
    let mut failures = Vec::new();
    if let Some(path) = cgroup_path {
        if let Err(error) = thaw_cgroup(path) {
            failures.push(format!("cgroup thaw failed: {error}"));
        }
        if let Err(error) = unfreeze_binder() {
            failures.push(format!("binder unfreeze failed: {error}"));
        }
    }
    if let Err(error) = resume_signal() {
        failures.push(format!("SIGCONT failed: {error}"));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(DaemonError::system(failures.join("; ")))
    }
}

/// 锁内：读取做 hook 健康检查与配置同步所需的只读快照，并标记是否需要同步配置。
/// 真正的网络往返在锁外执行（见 `execute_hook_health_work`），避免 3 秒同步调用
/// 期间占住全局 state Mutex 阻塞控制循环/管理器请求。
fn prepare_hook_health_work(state: &ReadOnlyState) -> HookHealthWork {
    // The hook has no durable session identifier. Re-send the current config
    // during every health cycle so a system_server/Xposed restart that happens
    // between probes cannot leave an old `hook_config_synced=true` behind.
    let needs_config_sync = true;
    let xposed_payload =
        encode_xposed_config_payload(&state.settings, &encode_app_config(&state.app_config)).ok();
    HookHealthWork {
        needs_config_sync,
        xposed_payload,
        config_revision: state.config_revision,
    }
}

struct HookHealthWork {
    needs_config_sync: bool,
    xposed_payload: Option<Vec<u8>>,
    config_revision: u64,
}

/// 锁外：执行 xposed bridge 的同步网络往返（3 秒超时）。不持有 state 锁。
fn execute_hook_health_work(
    work: &HookHealthWork,
    set_app_config: impl FnOnce(&[u8]) -> Result<bool, DaemonError>,
) -> HookHealthResult {
    let sync_result = if work.needs_config_sync {
        match &work.xposed_payload {
            Some(payload) => match set_app_config(payload) {
                Ok(synced) => Some(Ok(synced)),
                Err(error) => Some(Err(error)),
            },
            None => Some(Err(DaemonError::protocol(
                "failed to encode xposed config payload for sync",
            ))),
        }
    } else {
        None
    };
    let health_result = xposed_bridge::query_hook_health();
    HookHealthResult {
        sync_result,
        health_result,
    }
}

struct HookHealthResult {
    sync_result: Option<Result<bool, DaemonError>>,
    health_result: Result<String, DaemonError>,
}

fn xposed_config_matches_current_state(state: &ReadOnlyState, expected: Option<&[u8]>) -> bool {
    let Some(expected) = expected else {
        return false;
    };
    encode_xposed_config_payload(&state.settings, &encode_app_config(&state.app_config))
        .map(|current| current == expected)
        .unwrap_or(false)
}

/// 锁内：把锁外收集的结果写回 state。同步结果只在其配置快照仍等于当前
/// state 时生效，防止旧请求在新配置已经落盘后把 `hook_config_synced` 错置为 true。
fn apply_hook_health_result(
    state: &mut ReadOnlyState,
    work: HookHealthWork,
    result: HookHealthResult,
) {
    state.daemon_health = "active".to_owned();
    if let Some(sync_outcome) = result.sync_result {
        match sync_outcome {
            Ok(synced) => {
                let current = synced
                    && work.config_revision == state.config_revision
                    && xposed_config_matches_current_state(state, work.xposed_payload.as_deref());
                state.hook_config_synced = current;
                if current {
                    state.manager_log.push_once(LogRecord::diagnostic(
                        crate::app::logging::LogLevel::Debug,
                        now_ms(),
                        format!(
                            "hook config synced: managed_apps={} settings={}",
                            state.app_config.len(),
                            state.settings.len()
                        ),
                    ));
                } else if synced {
                    state.manager_log.push_once(LogRecord::fault(
                        crate::app::logging::LogLevel::Warn,
                        now_ms(),
                        "discarded stale hook config sync result",
                    ));
                } else {
                    state.manager_log.push_once(LogRecord::fault(
                        crate::app::logging::LogLevel::Warn,
                        now_ms(),
                        "hook config sync rejected",
                    ));
                }
            }
            Err(error) => {
                state.hook_config_synced = false;
                state.manager_log.push_once(LogRecord::fault(
                    crate::app::logging::LogLevel::Warn,
                    now_ms(),
                    format!("hook config sync failed: {error}"),
                ));
            }
        }
    }
    let hook_ready = match result.health_result {
        Ok(payload) => {
            let status = classify_hook_health_payload(&payload);
            state.hook_health = status.health_label().to_owned();
            state.xp_log = payload;
            status.is_ready_for_control()
        }
        Err(error) => {
            let status = classify_bridge_error(&error);
            state.hook_health = status.health_label().to_owned();
            state.xp_log = format!("hook bridge {}", status.health_label());
            false
        }
    };
    // A bridge outage or a degraded replacement hook can have lost all of its
    // in-memory configuration. The next health cycle must send SetConfig again.
    if !hook_ready {
        state.hook_config_synced = false;
    }
    refresh_runtime_diagnostics(state);
}

pub fn run_manager_server_forever(state: ReadOnlyState) -> Result<(), DaemonError> {
    let listener = bind_manager_listener()?;
    let state = Arc::new(Mutex::new(state));
    let control_state = Arc::new(Mutex::new(RuntimeControlState::default()));
    let policy_action_gate = Arc::new(Mutex::new(()));
    spawn_control_loop(
        state.clone(),
        control_state.clone(),
        policy_action_gate.clone(),
    );

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                // 单个请求的 panic 不得拖垮整个守护进程——daemon 退出会让所有经
                // SIGSTOP 冻结的应用无人恢复。捕获后记日志继续服务下一条连接。
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    serve_manager_connection(stream, &state, &policy_action_gate)
                }));
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => eprintln!("manager request failed: {error}"),
                    Err(panic) => eprintln!(
                        "manager request panicked: {}",
                        panic
                            .downcast_ref::<String>()
                            .map(|s| s.clone())
                            .or_else(|| {
                                panic.downcast_ref::<&'static str>().map(|s| s.to_string())
                            })
                            .unwrap_or_else(|| "unknown panic".to_string())
                    ),
                }
            }
            Err(error) => return Err(DaemonError::from(error)),
        }
    }

    Ok(())
}

fn spawn_control_loop(
    state: Arc<Mutex<ReadOnlyState>>,
    control_state: Arc<Mutex<RuntimeControlState>>,
    policy_action_gate: Arc<Mutex<()>>,
) {
    thread::spawn(move || loop {
        thread::sleep(std::time::Duration::from_secs(1));
        // This gate spans every policy-dependent action, including hook config
        // and runtime bridge work. A manager mutation either completes before
        // this pass snapshots policy, or waits until all actions derived from
        // this snapshot have finished.
        let _policy_action = match policy_action_gate.lock() {
            Ok(gate) => gate,
            Err(_) => {
                eprintln!("control loop policy action mutex poisoned");
                continue;
            }
        };
        // hook 健康检查与配置同步的 xposed bridge 网络往返最长 3 秒，必须在 state 锁
        // 之外执行，否则会阻塞管理器 TCP 请求。先在锁内取出工作描述，释放锁做网络，
        // 再重新加锁写回并 clone 出本轮控制所需的只读快照。
        let hook_work = match state.lock() {
            Ok(state) => prepare_hook_health_work(&state),
            Err(_) => {
                eprintln!("control loop state mutex poisoned");
                continue;
            }
        };
        let hook_result = execute_hook_health_work(&hook_work, xposed_bridge::set_config);
        let mut state_snapshot = match state.lock() {
            Ok(mut state) => {
                apply_hook_health_result(&mut state, hook_work, hook_result);
                state.clone()
            }
            Err(_) => {
                eprintln!("control loop state mutex poisoned");
                continue;
            }
        };
        let mut runtime_state = match control_state.lock() {
            Ok(control_state) => control_state,
            Err(_) => {
                eprintln!("control loop runtime state mutex poisoned");
                continue;
            }
        };
        if let Err(error) = run_live_control_pass(&mut state_snapshot, &mut runtime_state) {
            eprintln!("control loop pass failed: {error}");
        }
        let pending_uids = runtime_state.pending_freeze_uids();
        let network_uids = runtime_state.take_network_restriction_uids();
        let operation_log_json = runtime_state.operation_log.to_json();
        let operations = runtime_state
            .operation_log
            .records()
            .cloned()
            .collect::<Vec<_>>();
        drop(runtime_state);

        // Keep hook-side pending/broadcast state and network restriction state
        // synchronized without holding either daemon state mutex over socket I/O.
        let hook_runtime_outcome = if state_snapshot.hook_health == "active" {
            sync_hook_runtime_state(&pending_uids, &network_uids, xposed_bridge::request_bytes)
        } else {
            HookRuntimeSyncOutcome {
                pending_error: None,
                failed_network_uids: network_uids,
            }
        };
        if !hook_runtime_outcome.failed_network_uids.is_empty() {
            match control_state.lock() {
                Ok(mut control_state) => {
                    for uid in &hook_runtime_outcome.failed_network_uids {
                        control_state.requeue_network_restriction_uid(*uid);
                    }
                }
                Err(_) => eprintln!("control loop runtime state mutex poisoned"),
            }
        }

        let mut state = match state.lock() {
            Ok(state) => state,
            Err(_) => {
                eprintln!("control loop state mutex poisoned");
                continue;
            }
        };
        state.operation_log_json = operation_log_json;
        for operation in operations {
            state.manager_log.push(LogRecord::operation(operation));
        }
        if let Some(error) = hook_runtime_outcome.pending_error {
            state.manager_log.push_once(LogRecord::fault(
                crate::app::logging::LogLevel::Warn,
                now_ms(),
                format!("hook pending UID sync failed: {error}"),
            ));
        }
        if !hook_runtime_outcome.failed_network_uids.is_empty() {
            state.manager_log.push_once(LogRecord::fault(
                crate::app::logging::LogLevel::Warn,
                now_ms(),
                format!(
                    "hook network break failed for UIDs {:?}; queued for retry",
                    hook_runtime_outcome.failed_network_uids
                ),
            ));
        }
        state.freeze_status = state_snapshot.freeze_status;
    });
}

#[cfg(test)]
mod tests {
    use super::{
        apply_hook_health_result, prepare_hook_health_work, read_exact_with_deadline,
        run_unfreeze_sequence, should_run_control_pass, sync_hook_runtime_state,
        thaw_disabled_control_state, with_manager_state_after_request_read,
        with_policy_action_gate, HookHealthResult,
    };
    use crate::{
        app::{controller::run_control_pass, error::DaemonError},
        domain::runtime::{ControlState, ProcessState, RuntimeProcess},
        protocol::{
            manager_v1::{ManagerAppConfigRecord, ManagerFrame, ReadOnlyState},
            xposed::XposedCommand,
        },
    };
    use std::{
        cell::RefCell,
        io::{self, Read},
        sync::mpsc,
        sync::{Arc, Mutex},
        thread,
        time::{Duration, Instant},
    };

    struct DripReader {
        bytes: Vec<u8>,
        delay: Duration,
    }

    impl Read for DripReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.bytes.is_empty() {
                return Ok(0);
            }
            thread::sleep(self.delay);
            buffer[0] = self.bytes.remove(0);
            Ok(1)
        }
    }

    #[test]
    fn stale_hook_sync_result_does_not_mark_new_config_as_synced() {
        let mut state = ReadOnlyState::default();
        let work = prepare_hook_health_work(&state);
        state.settings[0] ^= 1;

        apply_hook_health_result(
            &mut state,
            work,
            HookHealthResult {
                sync_result: Some(Ok(true)),
                health_result: Ok("{\"status\":\"active\"}".to_owned()),
            },
        );

        assert!(!state.hook_config_synced);
    }

    #[test]
    fn hook_sync_result_rejects_a_reverted_payload_from_an_older_revision() {
        let mut state = ReadOnlyState::default();
        let work = prepare_hook_health_work(&state);

        // The encoded payload is equal again, but two committed policy changes
        // occurred while the Xposed request was in flight.
        state.settings[0] ^= 1;
        state.settings[0] ^= 1;
        state.config_revision = work.config_revision.wrapping_add(2);

        apply_hook_health_result(
            &mut state,
            work,
            HookHealthResult {
                sync_result: Some(Ok(true)),
                health_result: Ok("{\"status\":\"active\"}".to_owned()),
            },
        );

        assert!(!state.hook_config_synced);
    }

    #[test]
    fn unavailable_hook_invalidates_the_last_synced_config() {
        let mut state = ReadOnlyState {
            hook_config_synced: true,
            ..ReadOnlyState::default()
        };
        let work = prepare_hook_health_work(&state);

        apply_hook_health_result(
            &mut state,
            work,
            HookHealthResult {
                sync_result: None,
                health_result: Err(DaemonError::system("hook connection refused")),
            },
        );

        assert!(!state.hook_config_synced);
    }

    #[test]
    fn control_requires_a_confirmed_current_hook_config() {
        let mut state = ReadOnlyState {
            control_allowed: true,
            hook_health: "active".to_owned(),
            app_config: vec![ManagerAppConfigRecord {
                uid: 10_123,
                mode: 30,
                permissive: false,
            }],
            ..ReadOnlyState::default()
        };

        assert!(!should_run_control_pass(&state));
        state.hook_config_synced = true;
        assert!(should_run_control_pass(&state));
    }

    #[test]
    fn optional_hook_failure_keeps_active_system_control_eligible() {
        let mut state = ReadOnlyState {
            app_config: vec![ManagerAppConfigRecord {
                uid: 10_123,
                mode: 30,
                permissive: false,
            }],
            ..ReadOnlyState::default()
        };
        let work = prepare_hook_health_work(&state);

        apply_hook_health_result(
            &mut state,
            work,
            HookHealthResult {
                sync_result: Some(Ok(true)),
                health_result: Ok(
                    r#"{"status":"degraded","system_control_status":"active","hook_health":{"hook_status":"active","hooks":[{"id":"broadcast#modern_queue","critical":false,"stage":"registration"}]}}"#.to_owned(),
                ),
            },
        );

        assert_eq!(state.hook_health, "active");
        assert!(state.hook_config_synced);
        // Compatibility is independently derived from the real device. The host unit-test
        // runtime is intentionally not an approved Android target, so model a passed gate here.
        state.control_allowed = true;
        assert!(should_run_control_pass(&state));
    }

    #[test]
    fn whole_frame_deadline_rejects_a_drip_feed() {
        let mut reader = DripReader {
            bytes: vec![1, 2],
            delay: Duration::from_millis(40),
        };
        let mut target = [0_u8; 2];
        let result = read_exact_with_deadline(
            &mut reader,
            &mut target,
            Instant::now() + Duration::from_millis(70),
            |_, _| Ok(()),
        );

        let error = result.expect_err("the frame deadline must apply across partial reads");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn manager_request_read_precedes_the_shared_state_lock() {
        let state = Arc::new(Mutex::new(ReadOnlyState::default()));
        let during_read = state.clone();
        let result = with_manager_state_after_request_read(
            &state,
            || {
                let lock = during_read
                    .try_lock()
                    .expect("state must remain unlocked while request bytes are read");
                drop(lock);
                Err(DaemonError::protocol("incomplete manager request"))
            },
            |_, _| Ok(()),
        );

        assert!(result.is_err());
    }

    #[test]
    fn policy_handler_runs_without_holding_the_shared_state_lock() {
        let state = Arc::new(Mutex::new(ReadOnlyState::default()));
        let worker_state = state.clone();
        let (entered_sender, entered_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();

        let worker = thread::spawn(move || {
            with_manager_state_after_request_read(
                &worker_state,
                || {
                    Ok(ManagerFrame {
                        command: crate::protocol::manager_v1::ManagerCommand::SetSettingsVar,
                        payload: vec![10, 1],
                    })
                },
                |_, state| {
                    state.hook_config_synced = false;
                    entered_sender.send(()).expect("test receiver is alive");
                    release_receiver.recv().expect("test sender is alive");
                    Err::<(), _>(DaemonError::system("simulated policy handler failure"))
                },
            )
        });

        entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("policy handler started");
        let state_was_unlocked = state.try_lock().is_ok();
        release_sender.send(()).expect("worker is waiting");
        assert!(worker.join().expect("worker did not panic").is_err());
        assert!(
            !state
                .lock()
                .expect("state mutex remains usable")
                .hook_config_synced,
            "error-side rollback state must be committed after detached policy work"
        );
        assert!(
            state_was_unlocked,
            "blocking policy work must not retain the shared state mutex"
        );
    }

    #[test]
    fn policy_action_gate_prevents_config_work_from_overtaking_control_work() {
        let gate = Arc::new(Mutex::new(()));
        let control_gate = gate.clone();
        let config_gate = gate.clone();
        let (control_started_sender, control_started_receiver) = mpsc::channel();
        let (release_control_sender, release_control_receiver) = mpsc::channel();
        let (config_attempt_sender, config_attempt_receiver) = mpsc::channel();
        let (config_completed_sender, config_completed_receiver) = mpsc::channel();

        let control = thread::spawn(move || {
            with_policy_action_gate(&control_gate, || -> Result<(), DaemonError> {
                control_started_sender
                    .send(())
                    .expect("test receiver is alive");
                release_control_receiver
                    .recv()
                    .expect("test sender is alive");
                Ok(())
            })
        });

        control_started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("control work entered the policy gate");
        let config = thread::spawn(move || {
            config_attempt_sender
                .send(())
                .expect("test receiver is alive");
            with_policy_action_gate(&config_gate, || -> Result<(), DaemonError> {
                config_completed_sender
                    .send(())
                    .expect("test receiver is alive");
                Ok(())
            })
        });

        config_attempt_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("config work attempted the policy gate");
        assert!(
            matches!(
                config_completed_receiver.recv_timeout(Duration::from_millis(50)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "config work completed while an older control action still held the gate"
        );

        release_control_sender
            .send(())
            .expect("control work is waiting");
        control
            .join()
            .expect("control worker did not panic")
            .expect("control work succeeded");
        config
            .join()
            .expect("config worker did not panic")
            .expect("config work succeeded");
        config_completed_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("config work completed after control released the gate");
    }

    #[test]
    fn disabled_control_thaws_every_runtime_tracked_freeze() {
        let config = vec![ManagerAppConfigRecord {
            uid: 10_123,
            mode: 30,
            permissive: false,
        }];
        let process = RuntimeProcess {
            pid: 321,
            uid: 10_123,
            package_name: "example.app".to_owned(),
            process_name: "example.app".to_owned(),
            proc_state: ProcessState::Cached,
            control_state: ControlState::Running,
            cgroup_freeze_path: None,
            binder_state: None,
            start_time_ticks: Some(1),
            last_seen_at_ms: 0,
        };
        let mut runtime = crate::app::controller::RuntimeControlState::default();
        run_control_pass(
            &mut runtime,
            &config,
            |_, _| Ok(vec![process.clone()]),
            |_| Ok(()),
            |_| Ok(()),
            &[],
            1_000,
        )
        .expect("freeze is tracked");

        let calls = RefCell::new(Vec::new());
        let mut state = ReadOnlyState::default();
        thaw_disabled_control_state(
            &mut state,
            &mut runtime,
            |_, _| Ok(vec![process.clone()]),
            |process| {
                calls.borrow_mut().push(process.pid);
                Ok(())
            },
            |_| Ok(true),
            2_000,
        )
        .expect("tracked process thaws when control is disabled");

        assert_eq!(&*calls.borrow(), &[321]);
    }

    #[test]
    fn runtime_hook_sync_updates_pending_and_requeues_failed_network_breaks() {
        let calls = RefCell::new(Vec::new());
        let outcome =
            sync_hook_runtime_state(&[10_001, 10_002], &[10_003, 10_004], |command, payload| {
                calls.borrow_mut().push((command, payload.to_vec()));
                let reply = if command == XposedCommand::BreakNetwork
                    && payload == 10_004_u32.to_le_bytes()
                {
                    0_i32.to_le_bytes().to_vec()
                } else {
                    2_i32.to_le_bytes().to_vec()
                };
                Ok(reply)
            });

        assert!(outcome.pending_error.is_none());
        assert_eq!(outcome.failed_network_uids, vec![10_004]);
        assert_eq!(
            &*calls.borrow(),
            &[
                (
                    XposedCommand::UpdatePending,
                    [10_001_u32.to_le_bytes(), 10_002_u32.to_le_bytes()].concat(),
                ),
                (
                    XposedCommand::BreakNetwork,
                    10_003_u32.to_le_bytes().to_vec()
                ),
                (
                    XposedCommand::BreakNetwork,
                    10_004_u32.to_le_bytes().to_vec()
                ),
            ]
        );
    }

    #[test]
    fn cgroup_unfreeze_always_finishes_with_sigcont() {
        let calls = RefCell::new(Vec::new());
        run_unfreeze_sequence(
            Some("/tmp/cgroup.freeze"),
            |_| {
                calls.borrow_mut().push("cgroup");
                Ok(())
            },
            || {
                calls.borrow_mut().push("binder");
                Ok(())
            },
            || {
                calls.borrow_mut().push("sigcont");
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(&*calls.borrow(), &["cgroup", "binder", "sigcont"]);
    }

    #[test]
    fn signal_only_unfreeze_sends_sigcont_without_other_backends() {
        let calls = RefCell::new(Vec::new());
        run_unfreeze_sequence(
            None,
            |_| -> Result<(), DaemonError> {
                calls.borrow_mut().push("cgroup");
                Ok(())
            },
            || {
                calls.borrow_mut().push("binder");
                Ok(())
            },
            || {
                calls.borrow_mut().push("sigcont");
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(&*calls.borrow(), &["sigcont"]);
    }

    #[test]
    fn sigcont_is_attempted_when_cgroup_and_binder_unfreeze_fail() {
        let calls = RefCell::new(Vec::new());
        let error = run_unfreeze_sequence(
            Some("/tmp/cgroup.freeze"),
            |_| {
                calls.borrow_mut().push("cgroup");
                Err(DaemonError::system("thaw"))
            },
            || {
                calls.borrow_mut().push("binder");
                Err(DaemonError::system("binder"))
            },
            || {
                calls.borrow_mut().push("sigcont");
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(&*calls.borrow(), &["cgroup", "binder", "sigcont"]);
        assert!(error.to_string().contains("cgroup thaw failed"));
        assert!(error.to_string().contains("binder unfreeze failed"));
    }
}
