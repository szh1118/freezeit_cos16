use freezeit_daemon::{
    app::{
        controller::{
            decide_freeze, decide_freeze_after_reconciliation, mark_frozen, mark_running,
            run_control_pass, run_control_pass_with_settings, run_control_pass_with_validation,
            RuntimeControlState,
        },
        error::DaemonError,
        freezer_backend::{
            mark_processes_frozen, mark_processes_running, BackendEnvironment,
            SystemAwareCgroupBinderBackend,
        },
        package_inventory::PackageRecord,
        scheduler::FreezeScheduler,
    },
    domain::{
        policy::{
            FallbackAction, ForegroundStrategy, FreezeMode, FreezePolicy, ManagedApp,
            ProtectedReason,
        },
        runtime::{ControlState, ProcessState, RuntimeProcess},
    },
    protocol::manager_v1::ManagerAppConfigRecord,
    sys::socket::{require_foreground_uids_for_control, should_run_control_pass},
};
use std::{cell::RefCell, collections::BTreeMap};

#[test]
fn background_delay_reaches_pending_then_frozen_and_foreground_cancels() {
    let app = app();
    let mut scheduler = FreezeScheduler::default();
    scheduler.schedule_background(&app.package_name, app.uid, 100, 5000);

    assert!(scheduler.due_at(5099).is_empty());
    assert_eq!(scheduler.due_at(5100).len(), 1);
    assert!(scheduler
        .cancel_foreground(&app.package_name, app.uid)
        .is_some());
    assert!(scheduler.due_at(6000).is_empty());
}

#[test]
fn controller_decides_and_marks_freeze_unfreeze_flow() {
    let app = app();
    let policy = FreezePolicy::Selected {
        mode: FreezeMode::Freeze,
        delay_ms: 0,
        foreground_strategy: ForegroundStrategy::Strict,
        allow_network_restriction: false,
        allow_wakelock_restriction: false,
        fallback_strategy: vec![FallbackAction::Skip],
        updated_at_ms: 0,
    };
    let mut processes = vec![process()];

    assert_eq!(
        decide_freeze(&app, &policy, &processes).action,
        freezeit_daemon::app::freezer_backend::DecisionAction::Freeze
    );
    mark_frozen(&mut processes);
    assert_eq!(processes[0].control_state, ControlState::Frozen);
    mark_running(&mut processes);
    assert_eq!(processes[0].control_state, ControlState::Running);
}

#[test]
fn controller_rejects_stale_uid_before_freeze_control() {
    let app = app();
    let current_package = PackageRecord {
        package_name: app.package_name.clone(),
        user_id: app.user_id,
        uid: 10_999,
        label: app.label.clone(),
        is_system_app: false,
    };
    let policy = FreezePolicy::Selected {
        mode: FreezeMode::Freeze,
        delay_ms: 0,
        foreground_strategy: ForegroundStrategy::Strict,
        allow_network_restriction: false,
        allow_wakelock_restriction: false,
        fallback_strategy: vec![FallbackAction::Skip],
        updated_at_ms: 0,
    };

    let error = decide_freeze_after_reconciliation(&app, &current_package, &policy, &[process()])
        .expect_err("stale uid must block control");

    assert!(error.to_string().contains("uid changed"));
}

#[test]
fn runtime_status_snapshot_reports_frozen_and_foreground_rows() {
    let mut state = RuntimeControlState::default();
    let config = vec![ManagerAppConfigRecord {
        uid: 10_123,
        mode: 30,
        permissive: false,
    }];
    let processes = vec![process()];

    run_control_pass(
        &mut state,
        &config,
        |_, _| Ok(processes.clone()),
        |_| Ok(()),
        |_| Ok(()),
        &[],
        1_000,
    )
    .expect("freeze succeeds");

    let processes_by_uid = BTreeMap::from([(10_123, processes.clone())]);
    let frozen = state.freeze_status_records(&config, &processes_by_uid, &[], 4_000);
    assert_eq!(frozen.len(), 1);
    assert_eq!(frozen[0].uid, 10_123);
    assert!(!frozen[0].foreground);
    assert_eq!(frozen[0].state, 3);
    assert_eq!(frozen[0].seconds, 0);
    assert_eq!(frozen[0].process_count, 1);

    let foreground = state.freeze_status_records(&config, &processes_by_uid, &[10_123], 5_000);
    assert_eq!(foreground.len(), 1);
    assert!(foreground[0].foreground);
    assert_eq!(foreground[0].state, 1);
    assert_eq!(foreground[0].seconds, 0);
}

#[test]
fn freeze_and_unfreeze_operations_update_process_control_state() {
    let app = app();
    let mut processes = vec![process()];
    let backend = SystemAwareCgroupBinderBackend::new(BackendEnvironment::default());

    let freeze = backend.freeze_operation(&app, &processes, "delay elapsed");
    assert_eq!(freeze.pid_list, vec![123]);
    mark_processes_frozen(&mut processes);
    assert_eq!(processes[0].control_state, ControlState::Frozen);

    let unfreeze = backend.unfreeze_operation(&app, &processes, "foreground");
    assert_eq!(unfreeze.pid_list, vec![123]);
    mark_processes_running(&mut processes);
    assert_eq!(processes[0].control_state, ControlState::Running);
}

#[test]
fn control_pass_logs_freeze_then_unfreeze_for_configured_app() {
    let mut state = RuntimeControlState::default();
    let config = vec![ManagerAppConfigRecord {
        uid: 10_123,
        mode: 30,
        permissive: false,
    }];
    let processes = vec![process()];

    run_control_pass(
        &mut state,
        &config,
        |package_name, uid| {
            assert_eq!(package_name, "uid10123");
            assert_eq!(uid, 10_123);
            Ok(processes.clone())
        },
        |_| Ok(()),
        |_| Ok(()),
        &[],
        1000,
    )
    .expect("background pass succeeds");

    let json = state.operation_log.to_json();
    assert!(json.contains("\"packageName\":\"com.example.app\""));
    assert!(json.contains("\"action\":\"freeze\""));
    assert!(json.contains("\"result\":\"success\""));

    run_control_pass(
        &mut state,
        &config,
        |_, _| Ok(processes.clone()),
        |_| Ok(()),
        |_| Ok(()),
        &[10_123],
        2000,
    )
    .expect("foreground pass succeeds");

    let json = state.operation_log.to_json();
    assert!(json.contains("\"action\":\"unfreeze\""));
    assert!(json.contains("\"reason\":\"foreground uid active\""));
}

#[test]
fn control_pass_uses_manager_freeze_delay_before_freezing() {
    let mut state = RuntimeControlState::default();
    let config = vec![ManagerAppConfigRecord {
        uid: 10_123,
        mode: 30,
        permissive: false,
    }];
    let mut settings = freezeit_daemon::protocol::manager_v1::legacy_default_settings();
    settings[2] = 3;
    let mut freeze_calls = 0;

    run_control_pass_with_settings(
        &mut state,
        &config,
        &settings,
        |_, _| Ok(vec![process()]),
        |_| {
            freeze_calls += 1;
            Ok(())
        },
        |_| Ok(()),
        &[],
        1000,
    )
    .expect("first background pass schedules pending freeze");

    assert_eq!(freeze_calls, 0);
    assert!(state
        .operation_log
        .to_json()
        .contains("\"action\":\"postpone\""));
    assert!(state
        .operation_log
        .to_json()
        .contains("pending freeze delay 3000ms"));

    run_control_pass_with_settings(
        &mut state,
        &config,
        &settings,
        |_, _| Ok(vec![process()]),
        |_| {
            freeze_calls += 1;
            Ok(())
        },
        |_| Ok(()),
        &[],
        3999,
    )
    .expect("not due yet");

    assert_eq!(freeze_calls, 0);

    run_control_pass_with_settings(
        &mut state,
        &config,
        &settings,
        |_, _| Ok(vec![process()]),
        |_| {
            freeze_calls += 1;
            Ok(())
        },
        |_| Ok(()),
        &[],
        4000,
    )
    .expect("due control pass freezes");

    assert_eq!(freeze_calls, 1);
    assert!(state
        .operation_log
        .to_json()
        .contains("\"action\":\"freeze\""));
}

#[test]
fn control_pass_records_partial_freeze_when_uid_rescan_finds_new_processes() {
    let mut state = RuntimeControlState::default();
    let config = vec![ManagerAppConfigRecord {
        uid: 10_123,
        mode: 30,
        permissive: false,
    }];
    let mut discovery_count = 0;

    run_control_pass(
        &mut state,
        &config,
        |_, _| {
            discovery_count += 1;
            if discovery_count == 1 {
                let mut initial_process = process();
                initial_process.cgroup_freeze_path =
                    Some("/sys/fs/cgroup/apps/uid_10123/pid_123/cgroup.freeze".to_owned());
                Ok(vec![initial_process])
            } else {
                let mut initial_process = process();
                initial_process.cgroup_freeze_path =
                    Some("/sys/fs/cgroup/apps/uid_10123/pid_123/cgroup.freeze".to_owned());
                let mut new_process = process();
                new_process.pid = 456;
                new_process.cgroup_freeze_path =
                    Some("/sys/fs/cgroup/apps/uid_10123/pid_456/cgroup.freeze".to_owned());
                Ok(vec![initial_process, new_process])
            }
        },
        |_| Ok(()),
        |_| Ok(()),
        &[],
        1000,
    )
    .expect("control pass succeeds");

    let json = state.operation_log.to_json();
    assert!(json.contains("\"result\":\"partial\""));
    assert!(json.contains("new same-uid process appeared after freeze"));
    assert!(json.contains("456"));
}

#[test]
fn control_pass_skips_non_control_policies_and_absent_processes_without_log_spam() {
    let mut state = RuntimeControlState::default();
    let config = vec![
        ManagerAppConfigRecord {
            uid: 10_001,
            mode: 40,
            permissive: false,
        },
        ManagerAppConfigRecord {
            uid: 10_002,
            mode: 50,
            permissive: false,
        },
        ManagerAppConfigRecord {
            uid: 10_003,
            mode: 30,
            permissive: false,
        },
        ManagerAppConfigRecord {
            uid: 10_123,
            mode: 20,
            permissive: false,
        },
    ];

    run_control_pass(
        &mut state,
        &config,
        |package_name, uid| {
            assert_ne!(uid, 10_001, "protected records must not be discovered");
            assert_ne!(
                uid, 10_002,
                "force-whitelist records must not be discovered"
            );
            if uid == 10_003 {
                return Ok(Vec::new());
            }
            assert_eq!(package_name, "uid10123");
            Ok(vec![process()])
        },
        |_| Ok(()),
        |_| Ok(()),
        &[],
        1000,
    )
    .expect("control pass succeeds");

    let json = state.operation_log.to_json();
    assert!(json.contains("\"uid\":10123"));
    assert!(json.contains("\"action\":\"freeze\""));
    assert!(!json.contains("\"uid\":10001"));
    assert!(!json.contains("\"uid\":10002"));
    assert!(!json.contains("\"uid\":10003"));
}

#[test]
fn live_control_pass_requires_active_hook_and_configured_apps() {
    let mut state = freezeit_daemon::protocol::manager_v1::ReadOnlyState {
        hook_health: "active".to_owned(),
        control_allowed: true,
        app_config: vec![ManagerAppConfigRecord {
            uid: 10_123,
            mode: 30,
            permissive: false,
        }],
        ..Default::default()
    };

    assert!(should_run_control_pass(&state));

    state.hook_health = "degraded".to_owned();
    assert!(!should_run_control_pass(&state));

    state.hook_health = "active".to_owned();
    state.control_allowed = false;
    assert!(!should_run_control_pass(&state));

    state.control_allowed = true;
    state.app_config.clear();
    assert!(!should_run_control_pass(&state));

    state.app_config = vec![ManagerAppConfigRecord {
        uid: 10_123,
        mode: 40,
        permissive: false,
    }];
    assert!(!should_run_control_pass(&state));
}

#[test]
fn signal_batch_rolls_back_in_reverse_and_records_failed_when_rollback_completes() {
    let mut state = RuntimeControlState::default();
    let config = vec![ManagerAppConfigRecord {
        uid: 10_123,
        mode: 30,
        permissive: false,
    }];
    let mut first = process();
    first.pid = 101;
    first.cgroup_freeze_path = None;
    let mut second = first.clone();
    second.pid = 102;
    let mut third = first.clone();
    third.pid = 103;
    let processes = vec![first, second, third];
    let calls = RefCell::new(Vec::new());

    run_control_pass_with_validation(
        &mut state,
        &config,
        &[],
        |_, _| Ok(processes.clone()),
        |process| {
            calls.borrow_mut().push(("stop", process.pid));
            if process.pid == 103 {
                Err(DaemonError::system("stop failed"))
            } else {
                Ok(())
            }
        },
        |process| {
            calls.borrow_mut().push(("cont", process.pid));
            Ok(())
        },
        |_| Ok(true),
        &[],
        1000,
    )
    .expect("pass records transaction failure");

    assert_eq!(
        *calls.borrow(),
        vec![
            ("stop", 101),
            ("stop", 102),
            ("stop", 103),
            ("cont", 102),
            ("cont", 101)
        ]
    );
    let json = state.operation_log.to_json();
    assert!(json.contains("\"result\":\"failed\""));
    assert!(json.contains("rolled_back=2"));
    assert!(json.contains("control apply failed for pid 103"));
    assert!(!json.contains("signal failed for pid 103"));
}

#[test]
fn signal_batch_identity_drift_blocks_shared_uid_target_and_records_partial_on_rollback_failure() {
    let mut state = RuntimeControlState::default();
    let config = vec![ManagerAppConfigRecord {
        uid: 10_123,
        mode: 30,
        permissive: false,
    }];
    let mut first = process();
    first.pid = 201;
    first.cgroup_freeze_path = None;
    let mut shared_uid_other_package = first.clone();
    shared_uid_other_package.pid = 202;
    let processes = vec![first, shared_uid_other_package];
    let calls = RefCell::new(Vec::new());
    let validation_count = RefCell::new(0);

    run_control_pass_with_validation(
        &mut state,
        &config,
        &[],
        |_, _| Ok(processes.clone()),
        |process| {
            calls.borrow_mut().push(("stop", process.pid));
            Ok(())
        },
        |process| {
            calls.borrow_mut().push(("cont", process.pid));
            Err(DaemonError::system("rollback failed"))
        },
        |_| {
            let mut count = validation_count.borrow_mut();
            *count += 1;
            Ok(*count != 4)
        },
        &[],
        1000,
    )
    .expect("pass records identity failure");

    assert_eq!(*calls.borrow(), vec![("stop", 201), ("cont", 201)]);
    let json = state.operation_log.to_json();
    assert!(json.contains("\"result\":\"partial\""));
    assert!(json.contains("identity validation failed"));
}

#[test]
fn signal_batch_rejects_shared_uid_packages_before_any_signal() {
    let mut state = RuntimeControlState::default();
    let config = vec![ManagerAppConfigRecord {
        uid: 10_123,
        mode: 30,
        permissive: false,
    }];
    let mut first = process();
    first.pid = 301;
    first.cgroup_freeze_path = None;
    let mut other = first.clone();
    other.pid = 302;
    other.package_name = "com.other.shared".to_owned();
    other.process_name = "com.other.shared".to_owned();
    let signals = RefCell::new(Vec::new());

    run_control_pass_with_validation(
        &mut state,
        &config,
        &[],
        |_, _| Ok(vec![first.clone(), other.clone()]),
        |process| {
            signals.borrow_mut().push(("stop", process.pid));
            Ok(())
        },
        |process| {
            signals.borrow_mut().push(("cont", process.pid));
            Ok(())
        },
        |_| Ok(true),
        &[],
        1000,
    )
    .expect("shared uid is rejected safely");

    run_control_pass_with_validation(
        &mut state,
        &config,
        &[],
        |_, _| Ok(vec![first.clone(), other.clone()]),
        |process| {
            signals.borrow_mut().push(("stop", process.pid));
            Ok(())
        },
        |process| {
            signals.borrow_mut().push(("cont", process.pid));
            Ok(())
        },
        |_| Ok(true),
        &[],
        2_000,
    )
    .expect("unchanged shared uid rejection is suppressed");

    assert!(signals.borrow().is_empty());
    assert_eq!(state.operation_log.records().count(), 1);
    let json = state.operation_log.to_json();
    assert!(json.contains("shared uid contains multiple package identities"));
    assert!(json.contains("\"result\":\"failed\""));
}

#[test]
fn shared_uid_rejection_is_deduplicated_when_proc_order_changes() {
    let mut state = RuntimeControlState::default();
    let config = vec![ManagerAppConfigRecord {
        uid: 10_123,
        mode: 30,
        permissive: false,
    }];
    let mut first = process();
    first.pid = 301;
    first.cgroup_freeze_path = None;
    let mut other = first.clone();
    other.pid = 302;
    other.package_name = "com.other.shared".to_owned();
    other.process_name = "com.other.shared".to_owned();

    for processes in [
        vec![first.clone(), other.clone()],
        vec![other.clone(), first.clone()],
    ] {
        run_control_pass_with_validation(
            &mut state,
            &config,
            &[],
            |_, _| Ok(processes.clone()),
            |_| Ok(()),
            |_| Ok(()),
            |_| Ok(true),
            &[],
            1_000,
        )
        .expect("shared uid rejection remains safe");
    }

    assert_eq!(state.operation_log.records().count(), 1);
}

#[test]
fn process_exit_clears_pending_freeze_status() {
    let mut state = RuntimeControlState::default();
    let config = vec![ManagerAppConfigRecord {
        uid: 10_123,
        mode: 30,
        permissive: false,
    }];
    let mut settings = freezeit_daemon::protocol::manager_v1::legacy_default_settings();
    settings[2] = 3;

    run_control_pass_with_settings(
        &mut state,
        &config,
        &settings,
        |_, _| Ok(vec![process()]),
        |_| Ok(()),
        |_| Ok(()),
        &[],
        0,
    )
    .expect("first pass schedules pending freeze");

    run_control_pass_with_settings(
        &mut state,
        &config,
        &settings,
        |_, _| Ok(Vec::new()),
        |_| Ok(()),
        |_| Ok(()),
        &[],
        1_000,
    )
    .expect("empty process pass clears stale state");

    let rows = state.freeze_status_records(&config, &BTreeMap::new(), &[], 1_000);
    assert!(rows.is_empty());
}

#[test]
fn terminal_shared_uid_failure_clears_expired_pending_status() {
    let mut state = RuntimeControlState::default();
    let config = vec![ManagerAppConfigRecord {
        uid: 10_123,
        mode: 30,
        permissive: false,
    }];
    let mut settings = vec![0_u8; 256];
    settings[2] = 1;
    let mut first = process();
    first.cgroup_freeze_path = Some("/sys/fs/cgroup/uid_10123/cgroup.freeze".to_owned());
    let mut other = first.clone();
    other.pid = 302;
    other.package_name = "com.other.shared".to_owned();
    other.process_name = "com.other.shared".to_owned();
    let processes = vec![first, other];

    run_control_pass_with_validation(
        &mut state,
        &config,
        &settings,
        |_, _| Ok(processes.clone()),
        |_| Ok(()),
        |_| Ok(()),
        |_| Ok(true),
        &[],
        0,
    )
    .expect("first pass schedules freeze");
    run_control_pass_with_validation(
        &mut state,
        &config,
        &settings,
        |_, _| Ok(processes.clone()),
        |_| Ok(()),
        |_| Ok(()),
        |_| Ok(true),
        &[],
        2_000,
    )
    .expect("expired pending freeze is rejected safely");

    let rows =
        state.freeze_status_records(&config, &BTreeMap::from([(10_123, processes)]), &[], 2_000);
    assert_eq!(rows.len(), 1);
    assert_ne!(rows[0].state, 2);
}

#[test]
fn live_control_pass_treats_foreground_query_failure_as_fail_closed() {
    let error =
        require_foreground_uids_for_control(Err(DaemonError::system("foreground query failed")))
            .expect_err("foreground query failure must block control");

    assert!(error.to_string().contains("foreground query failed"));
}

fn app() -> ManagedApp {
    ManagedApp {
        package_name: "com.example.app".to_owned(),
        user_id: 0,
        uid: 10_123,
        label: "Example".to_owned(),
        is_system_app: false,
        protected_reason: None::<ProtectedReason>,
        policy_id: "default".to_owned(),
        last_seen_baseline: "test".to_owned(),
    }
}

fn process() -> RuntimeProcess {
    RuntimeProcess {
        pid: 123,
        uid: 10_123,
        package_name: "com.example.app".to_owned(),
        process_name: "com.example.app".to_owned(),
        proc_state: ProcessState::Cached,
        control_state: ControlState::Running,
        cgroup_freeze_path: None,
        binder_state: None,
        start_time_ticks: Some(1),
        last_seen_at_ms: 0,
    }
}
