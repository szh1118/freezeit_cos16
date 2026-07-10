use freezeit_daemon::{
    app::controller::{run_control_pass_with_sampling, RuntimeControlState},
    domain::runtime::{ControlState, ProcessState, RuntimeProcess},
    protocol::manager_v1::ManagerAppConfigRecord,
};

fn process(control_state: ControlState) -> RuntimeProcess {
    RuntimeProcess {
        pid: 123,
        uid: 10_123,
        package_name: "com.example.app".to_owned(),
        process_name: "com.example.app".to_owned(),
        proc_state: ProcessState::Cached,
        control_state,
        cgroup_freeze_path: Some("/sys/fs/cgroup/uid_10123/cgroup.freeze".to_owned()),
        binder_state: Some("idle".to_owned()),
        start_time_ticks: Some(1),
        last_seen_at_ms: 0,
    }
}

fn config() -> Vec<ManagerAppConfigRecord> {
    vec![ManagerAppConfigRecord {
        uid: 10_123,
        mode: 30,
        permissive: false,
    }]
}

#[test]
fn startup_audit_refreezes_abnormal_thaw_every_sixty_seconds() {
    let mut state = RuntimeControlState::default();
    let mut freezes = 0;
    run_control_pass_with_sampling(
        &mut state,
        &config(),
        &[],
        |_, _| Ok(vec![process(ControlState::Frozen)]),
        |_| {
            freezes += 1;
            Ok(())
        },
        |_| Ok(()),
        |_| Ok(true),
        |_, _| Ok(None),
        &[],
        0,
    )
    .unwrap();
    run_control_pass_with_sampling(
        &mut state,
        &config(),
        &[],
        |_, _| Ok(vec![process(ControlState::Running)]),
        |_| {
            freezes += 1;
            Ok(())
        },
        |_| Ok(()),
        |_| Ok(true),
        |_, _| Ok(None),
        &[],
        60_000,
    )
    .unwrap();

    assert_eq!(freezes, 2);
    assert!(state
        .operation_log
        .to_json()
        .contains("abnormal thaw audit"));
}

#[test]
fn foreground_uid_is_never_refrozen_by_audit() {
    let mut state = RuntimeControlState::default();
    let mut freezes = 0;
    run_control_pass_with_sampling(
        &mut state,
        &config(),
        &[],
        |_, _| Ok(vec![process(ControlState::Frozen)]),
        |_| {
            freezes += 1;
            Ok(())
        },
        |_| Ok(()),
        |_| Ok(true),
        |_, _| Ok(None),
        &[],
        0,
    )
    .unwrap();
    run_control_pass_with_sampling(
        &mut state,
        &config(),
        &[],
        |_, _| Ok(vec![process(ControlState::Running)]),
        |_| {
            freezes += 1;
            Ok(())
        },
        |_| Ok(()),
        |_| Ok(true),
        |_, _| Ok(None),
        &[10_123],
        60_000,
    )
    .unwrap();

    assert_eq!(freezes, 1);
}

#[test]
fn regular_refreeze_setting_controls_post_startup_interval() {
    let mut state = RuntimeControlState::default();
    let mut settings = freezeit_daemon::protocol::manager_v1::legacy_default_settings();
    settings[6] = 1;
    let mut freezes = 0;
    run_control_pass_with_sampling(
        &mut state,
        &config(),
        &settings,
        |_, _| Ok(vec![process(ControlState::Frozen)]),
        |_| {
            freezes += 1;
            Ok(())
        },
        |_| Ok(()),
        |_| Ok(true),
        |_, _| Ok(None),
        &[],
        0,
    )
    .unwrap();
    run_control_pass_with_sampling(
        &mut state,
        &config(),
        &settings,
        |_, _| Ok(vec![process(ControlState::Frozen)]),
        |_| {
            freezes += 1;
            Ok(())
        },
        |_| Ok(()),
        |_| Ok(true),
        |_, _| Ok(None),
        &[],
        900_000,
    )
    .unwrap();
    run_control_pass_with_sampling(
        &mut state,
        &config(),
        &settings,
        |_, _| Ok(vec![process(ControlState::Running)]),
        |_| {
            freezes += 1;
            Ok(())
        },
        |_| Ok(()),
        |_| Ok(true),
        |_, _| Ok(None),
        &[],
        2_699_999,
    )
    .unwrap();
    assert_eq!(freezes, 1);
    run_control_pass_with_sampling(
        &mut state,
        &config(),
        &settings,
        |_, _| Ok(vec![process(ControlState::Running)]),
        |_| {
            freezes += 1;
            Ok(())
        },
        |_| Ok(()),
        |_| Ok(true),
        |_, _| Ok(None),
        &[],
        2_700_000,
    )
    .unwrap();
    assert_eq!(freezes, 2);
}
