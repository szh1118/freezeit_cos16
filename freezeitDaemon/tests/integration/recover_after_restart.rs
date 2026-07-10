use freezeit_daemon::{
    app::controller::recover_after_restart,
    domain::{
        operation::{ControlAction, OperationResult},
        runtime::{ControlState, ProcessState, RuntimeProcess},
    },
};

#[test]
fn restart_recovery_records_current_process_state_before_new_control() {
    let processes = vec![RuntimeProcess {
        pid: 123,
        uid: 10_123,
        package_name: "com.example.app".to_owned(),
        process_name: "com.example.app:push".to_owned(),
        proc_state: ProcessState::Cached,
        control_state: ControlState::Frozen,
        cgroup_freeze_path: None,
        binder_state: None,
        start_time_ticks: Some(1),
        last_seen_at_ms: 900,
    }];

    let operation = recover_after_restart(9, 1_000, "com.example.app", 10_123, &processes);

    assert_eq!(operation.action, ControlAction::Recover);
    assert_eq!(operation.result, OperationResult::Success);
    assert_eq!(operation.pid_list, vec![123]);
    assert!(operation.details.contains("observed 1 process"));
}
