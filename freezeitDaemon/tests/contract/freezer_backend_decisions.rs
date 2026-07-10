use freezeit_daemon::{
    app::error::DaemonError,
    app::freezer_backend::{BackendEnvironment, DecisionAction, SystemAwareCgroupBinderBackend},
    domain::{
        policy::{FallbackAction, ForegroundStrategy, FreezeMode, FreezePolicy, ManagedApp},
        runtime::{ControlState, ProcessState, RuntimeProcess},
    },
};

#[test]
fn freezes_when_cgroup_binder_hook_and_screen_evidence_are_available() {
    let backend = SystemAwareCgroupBinderBackend::new(BackendEnvironment::default());
    let decision = backend.can_freeze(&app(), &policy(vec![FallbackAction::Skip]), &[process()]);

    assert_eq!(decision.action, DecisionAction::Freeze);
}

#[test]
fn postpones_when_screen_state_or_hook_evidence_is_unavailable() {
    let backend = SystemAwareCgroupBinderBackend::new(BackendEnvironment {
        screen_state_available: false,
        ..BackendEnvironment::default()
    });

    let decision = backend.can_freeze(&app(), &policy(vec![FallbackAction::Skip]), &[process()]);

    assert_eq!(decision.action, DecisionAction::Postpone);
    assert!(decision.reason.contains("screen-state"));
}

#[test]
fn follows_configured_fallback_when_preferred_freezer_is_unavailable() {
    let backend = SystemAwareCgroupBinderBackend::new(BackendEnvironment {
        cgroup_available: false,
        binder_available: false,
        network_available: false,
        wakelock_available: false,
        ..BackendEnvironment::default()
    });

    let decision = backend.can_freeze(
        &app(),
        &policy(vec![FallbackAction::AlternateFreezer, FallbackAction::Skip]),
        &[process()],
    );

    assert_eq!(decision.action, DecisionAction::AlternateFreezer);
}

#[test]
fn fallback_order_uses_the_first_configured_safe_action() {
    let backend = SystemAwareCgroupBinderBackend::new(BackendEnvironment {
        cgroup_available: false,
        binder_available: false,
        ..BackendEnvironment::default()
    });

    for (fallback, expected) in [
        (FallbackAction::Postpone, DecisionAction::Postpone),
        (
            FallbackAction::AlternateFreezer,
            DecisionAction::AlternateFreezer,
        ),
        (FallbackAction::Signal, DecisionAction::Signal),
        (FallbackAction::Terminate, DecisionAction::Terminate),
        (FallbackAction::Skip, DecisionAction::Skip),
    ] {
        let decision = backend.can_freeze(&app(), &policy(vec![fallback]), &[process()]);
        assert_eq!(decision.action, expected);
    }
}

#[test]
fn permission_denied_freezer_write_uses_configured_fallback_order() {
    let backend = SystemAwareCgroupBinderBackend::new(BackendEnvironment::default());
    let error = DaemonError::from(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "cgroup.freeze denied",
    ));

    let decision = backend.fallback_after_freeze_apply_error(
        &policy(vec![FallbackAction::Postpone, FallbackAction::Skip]),
        &error,
    );

    assert_eq!(decision.action, DecisionAction::Postpone);
    assert!(decision.reason.contains("permission denied"));
    assert!(decision.reason.contains("cgroup.freeze denied"));
}

#[test]
fn pending_freeze_is_postponed_when_binder_or_process_evidence_is_busy() {
    let backend = SystemAwareCgroupBinderBackend::new(BackendEnvironment::default());
    let mut process = process();
    process.control_state = ControlState::PendingFreeze;
    process.binder_state = Some("sync_txns_pending".to_owned());

    let decision = backend.can_freeze(&app(), &policy(vec![FallbackAction::Skip]), &[process]);

    assert_eq!(decision.action, DecisionAction::Postpone);
    assert!(decision.reason.contains("idle evidence"));
}

fn app() -> ManagedApp {
    ManagedApp {
        package_name: "com.example.app".to_owned(),
        user_id: 0,
        uid: 10_123,
        label: "Example".to_owned(),
        is_system_app: false,
        protected_reason: None,
        policy_id: "default".to_owned(),
        last_seen_baseline: "test".to_owned(),
    }
}

fn policy(fallback_strategy: Vec<FallbackAction>) -> FreezePolicy {
    FreezePolicy::Selected {
        mode: FreezeMode::Freeze,
        delay_ms: 1000,
        foreground_strategy: ForegroundStrategy::Strict,
        allow_network_restriction: false,
        allow_wakelock_restriction: false,
        fallback_strategy,
        updated_at_ms: 0,
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
