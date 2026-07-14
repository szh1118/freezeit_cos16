use std::{
    collections::BTreeSet,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    app::{
        controller::{
            is_control_policy_mode, refresh_runtime_diagnostics, run_control_pass_with_sampling,
            RuntimeControlState,
        },
        error::DaemonError,
        logging::LogRecord,
    },
    protocol::{
        manager_v1::{
            encode_app_config, encode_frame, encode_xposed_config_payload,
            handle_manager_command, parse_frame, ReadOnlyState, HEADER_LEN,
            MANAGER_LISTEN_HOST, MANAGER_LISTEN_PORT, MAX_PAYLOAD_LEN,
        },
        xposed::{classify_bridge_error, classify_hook_health_payload},
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

pub fn handle_single_manager_stream(
    mut stream: TcpStream,
    state: &mut ReadOnlyState,
) -> Result<(), DaemonError> {
    let mut header = [0_u8; HEADER_LEN];
    // 给整条流设置超时，避免慢/恶意客户端在 read_exact 上永久阻塞并占住 state 锁。
    let _ = stream.set_read_timeout(Some(MANAGER_STREAM_TIMEOUT));
    let _ = stream.set_write_timeout(Some(MANAGER_STREAM_TIMEOUT));
    stream.read_exact(&mut header)?;
    let payload_len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    // 必须在 resize 之前校验长度上限，否则恶意 0xFFFFFFFF 会触发 ~4GB 分配 + 永久阻塞读。
    if payload_len > MAX_PAYLOAD_LEN {
        return Err(DaemonError::protocol(format!(
            "manager frame payload too large: {payload_len} > {MAX_PAYLOAD_LEN}"
        )));
    }
    let mut bytes = header.to_vec();
    bytes.resize(HEADER_LEN + payload_len, 0);
    stream.read_exact(&mut bytes[HEADER_LEN..])?;

    let request = parse_frame(&bytes)?;
    // 注意：不再在每次 manager 请求时同步刷新 hook 健康——控制循环已每秒在锁外
    // 执行 xposed bridge 网络往返并写回 state，这里再刷一次会把 3 秒同步调用留在
    // state 锁内，阻塞控制循环。manager 端读取的 hook_health 最多滞后 1 秒，可接受。
    let payload = handle_manager_command(&request, state, xposed_bridge::set_config)?;
    let response = encode_frame(request.command, &payload)?;
    stream.write_all(&response)?;
    Ok(())
}

fn run_live_control_pass(
    state: &mut ReadOnlyState,
    control_state: &mut RuntimeControlState,
) -> Result<(), DaemonError> {
    if !should_run_control_pass(state) {
        state.freeze_status.clear();
        return Ok(());
    }

    let control_uids = state
        .app_config
        .iter()
        .filter(|record| is_control_policy_mode(record.mode))
        .map(|record| record.uid)
        .collect::<BTreeSet<_>>();
    let processes_by_uid =
        procfs::discover_managed_uid_processes(procfs::PROC_ROOT, &control_uids)?;
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

pub fn should_run_control_pass(state: &ReadOnlyState) -> bool {
    state.control_allowed
        && state.hook_health == "active"
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
    let needs_config_sync = !state.hook_config_synced;
    let xposed_payload = if needs_config_sync {
        encode_xposed_config_payload(&state.settings, &encode_app_config(&state.app_config)).ok()
    } else {
        None
    };
    HookHealthWork {
        needs_config_sync,
        xposed_payload,
    }
}

struct HookHealthWork {
    needs_config_sync: bool,
    xposed_payload: Option<Vec<u8>>,
}

/// 锁外：执行 xposed bridge 的同步网络往返（3 秒超时）。不持有 state 锁。
fn execute_hook_health_work(
    work: HookHealthWork,
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

/// 锁内：把锁外收集的结果写回 state。
fn apply_hook_health_result(state: &mut ReadOnlyState, result: HookHealthResult) {
    state.daemon_health = "active".to_owned();
    if let Some(sync_outcome) = result.sync_result {
        match sync_outcome {
            Ok(synced) => {
                state.hook_config_synced = synced;
                if synced {
                    state.manager_log.push_once(LogRecord::diagnostic(
                        crate::app::logging::LogLevel::Debug,
                        now_ms(),
                        format!(
                            "hook config synced: managed_apps={} settings={}",
                            state.app_config.len(),
                            state.settings.len()
                        ),
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
                state.manager_log.push_once(LogRecord::fault(
                    crate::app::logging::LogLevel::Warn,
                    now_ms(),
                    format!("hook config sync failed: {error}"),
                ));
            }
        }
    }
    match result.health_result {
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
    refresh_runtime_diagnostics(state);
}

pub fn run_manager_server_forever(state: ReadOnlyState) -> Result<(), DaemonError> {
    let listener = bind_manager_listener()?;
    let state = Arc::new(Mutex::new(state));
    let control_state = Arc::new(Mutex::new(RuntimeControlState::default()));
    spawn_control_loop(state.clone(), control_state.clone());

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let mut state = state
                    .lock()
                    .map_err(|_| DaemonError::system("manager state mutex poisoned"))?;
                // 单个请求的 panic 不得拖垮整个守护进程——daemon 退出会让所有经
                // SIGSTOP 冻结的应用无人恢复。捕获后记日志继续服务下一条连接。
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    handle_single_manager_stream(stream, &mut state)
                }));
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => eprintln!("manager request failed: {error}"),
                    Err(panic) => eprintln!(
                        "manager request panicked: {}",
                        panic.downcast_ref::<String>()
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
) {
    thread::spawn(move || loop {
        thread::sleep(std::time::Duration::from_secs(1));
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
        let hook_result = execute_hook_health_work(hook_work, xposed_bridge::set_config);
        let mut state_snapshot = match state.lock() {
            Ok(mut state) => {
                apply_hook_health_result(&mut state, hook_result);
                state.clone()
            }
            Err(_) => {
                eprintln!("control loop state mutex poisoned");
                continue;
            }
        };
        let mut control_state = match control_state.lock() {
            Ok(control_state) => control_state,
            Err(_) => {
                eprintln!("control loop runtime state mutex poisoned");
                continue;
            }
        };
        if let Err(error) = run_live_control_pass(&mut state_snapshot, &mut control_state) {
            eprintln!("control loop pass failed: {error}");
        }
        let operation_log_json = control_state.operation_log.to_json();
        let operations = control_state
            .operation_log
            .records()
            .cloned()
            .collect::<Vec<_>>();
        drop(control_state);

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
        state.freeze_status = state_snapshot.freeze_status;
    });
}

#[cfg(test)]
mod tests {
    use super::run_unfreeze_sequence;
    use crate::app::error::DaemonError;
    use std::cell::RefCell;

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
