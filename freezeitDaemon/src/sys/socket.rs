use std::{
    collections::BTreeSet,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    app::{
        controller::{
            is_control_policy_mode, refresh_runtime_diagnostics, run_control_pass_with_sampling,
            sync_loaded_config_to_hook, RuntimeControlState,
        },
        error::DaemonError,
    },
    protocol::{
        manager_v1::{
            encode_frame, handle_manager_command, parse_frame, ReadOnlyState, HEADER_LEN,
            MANAGER_LISTEN_HOST, MANAGER_LISTEN_PORT,
        },
        xposed::{classify_bridge_error, classify_hook_health_payload},
    },
    sys::{binder, cgroup, procfs, signal, xposed_bridge},
};

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
    stream.read_exact(&mut header)?;
    let payload_len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let mut bytes = header.to_vec();
    bytes.resize(HEADER_LEN + payload_len, 0);
    stream.read_exact(&mut bytes[HEADER_LEN..])?;

    let request = parse_frame(&bytes)?;
    refresh_hook_health(state);
    let payload = handle_manager_command(&request, state, xposed_bridge::set_config)?;
    let response = encode_frame(request.command, &payload)?;
    stream.write_all(&response)?;
    Ok(())
}

fn run_live_control_pass(
    state: &ReadOnlyState,
    control_state: &mut RuntimeControlState,
) -> Result<(), DaemonError> {
    if !should_run_control_pass(state) {
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
    )
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

fn refresh_hook_health(state: &mut ReadOnlyState) {
    state.daemon_health = "active".to_owned();
    if !state.hook_config_synced {
        let _ = sync_loaded_config_to_hook(state, xposed_bridge::set_config);
    }
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
                if let Err(error) = handle_single_manager_stream(stream, &mut state) {
                    eprintln!("manager request failed: {error}");
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
        let state_snapshot = match state.lock() {
            Ok(mut state) => {
                refresh_hook_health(&mut state);
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
        if let Err(error) = run_live_control_pass(&state_snapshot, &mut control_state) {
            eprintln!("control loop pass failed: {error}");
        }
        let operation_log_json = control_state.operation_log.to_json();
        let operation_log_text = control_state.operation_log.to_legacy_text();
        drop(control_state);

        let mut state = match state.lock() {
            Ok(state) => state,
            Err(_) => {
                eprintln!("control loop state mutex poisoned");
                continue;
            }
        };
        state.operation_log_json = operation_log_json;
        state.operation_log_text = operation_log_text;
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
