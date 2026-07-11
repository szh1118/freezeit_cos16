use freezeit_daemon::{
    app::{
        health::ModuleHealth,
        operation_log::{
            operation_to_debug_text, operation_to_json, operation_to_legacy_text, OperationLog,
        },
    },
    domain::{
        capability::{CapabilityName, ControlCapability},
        operation::{ControlAction, ControlOperation, OperationResult},
    },
    protocol::manager_v2::{capability_report_json, health_report_json, self_check_json},
};

#[test]
fn operation_log_json_contains_identity_action_result_and_reason() {
    let operation = sample_operation(ControlAction::Freeze, OperationResult::Success);
    let mut log = OperationLog::new(2);
    log.push(operation.clone());

    let json = log.to_json();

    assert!(json.contains("\"packageName\":\"com.example.app\""));
    assert!(json.contains("\"uid\":10123"));
    assert!(json.contains("\"action\":\"freeze\""));
    assert!(json.contains("\"result\":\"success\""));
    assert!(json.contains("\"reason\":\"delay elapsed\""));
    assert_eq!(
        operation_to_json(&operation)
            .matches("\"pidList\":[123,124]")
            .count(),
        1
    );
}

#[test]
fn operation_log_legacy_text_formats_freeze_like_original_manager_log() {
    let operation = sample_operation(ControlAction::Freeze, OperationResult::Success);

    let text = operation_to_legacy_text(&operation);

    assert_eq!(text, "[08:00:00]  ❄️冻结 com.example.app 2进程\n");
    assert!(!text.contains("UID:"));
    assert!(!text.contains("PID:"));
    assert!(!text.contains("方式:"));
    assert!(!text.contains("结果:"));
    assert!(!text.contains("原因:"));
    assert!(!text.contains("uid="));
    assert!(!text.contains("backend="));
    assert!(!text.contains("result="));
    assert!(!text.contains("reason="));
    assert!(!text.contains("operationId="));
    assert!(!text.contains("action=freeze"));

    let debug = operation_to_debug_text(&operation);
    assert!(debug.contains("UID:10123"));
    assert!(debug.contains("PID:123,124"));
    assert!(debug.contains("方式:cgroup-v2"));
    assert!(debug.contains("结果:成功"));
    assert!(debug.contains("原因:delay elapsed"));
}

#[test]
fn operation_log_legacy_text_formats_other_actions_with_legacy_symbols() {
    let mut log = OperationLog::new(8);
    log.push(sample_operation(
        ControlAction::Unfreeze,
        OperationResult::Success,
    ));
    log.push(sample_operation(
        ControlAction::Terminate,
        OperationResult::Skipped,
    ));
    log.push(sample_operation(
        ControlAction::Postpone,
        OperationResult::Postponed,
    ));
    log.push(sample_operation(
        ControlAction::Fallback,
        OperationResult::Skipped,
    ));
    log.push(sample_operation(
        ControlAction::Skip,
        OperationResult::Skipped,
    ));
    log.push(sample_operation(
        ControlAction::Recover,
        OperationResult::Success,
    ));

    let text = log.to_legacy_text();

    assert!(text.contains("☀️解冻 com.example.app 2进程"));
    assert!(text.contains("😭关闭 com.example.app 2进程"));
    assert!(text.contains("⏳延迟冻结 com.example.app 2进程"));
    assert!(text.contains("⚠️降级处理 com.example.app 2进程"));
    assert!(text.contains("⚠️跳过 com.example.app 2进程"));
    assert!(text.contains("♻️恢复 com.example.app 2进程"));
}

#[test]
fn operation_log_legacy_text_formats_launch_and_binder_blocker_like_cpp_log() {
    let launch = ControlOperation {
        pid_list: Vec::new(),
        action: ControlAction::Unfreeze,
        reason: "foreground uid active".to_owned(),
        result: OperationResult::Success,
        details: String::new(),
        ..sample_operation(ControlAction::Unfreeze, OperationResult::Success)
    };
    let binder_blocker = ControlOperation {
        action: ControlAction::Postpone,
        reason: "pending Binder transaction; retry in 15s".to_owned(),
        result: OperationResult::Postponed,
        details: "pid123:binder transaction active".to_owned(),
        ..sample_operation(ControlAction::Postpone, OperationResult::Postponed)
    };

    let text = format!(
        "{}{}",
        operation_to_legacy_text(&launch),
        operation_to_legacy_text(&binder_blocker)
    );

    assert!(text.contains("😁启动 com.example.app"));
    assert!(text.contains("com.example.app:123 Binder正在传输"));
    assert!(text.contains("后再冻结"));
    assert!(!text.contains("backend="));
    assert!(!text.contains("result="));
    assert!(!text.contains("reason="));
}

#[test]
fn operation_log_discards_oldest_records_when_capacity_is_reached() {
    let mut log = OperationLog::new(1);
    log.push(sample_operation(
        ControlAction::Postpone,
        OperationResult::Postponed,
    ));
    log.push(sample_operation(
        ControlAction::Skip,
        OperationResult::Skipped,
    ));

    let json = log.to_json();

    assert!(!json.contains("\"action\":\"postpone\""));
    assert!(json.contains("\"action\":\"skip\""));
}

#[test]
fn operation_log_persists_json_snapshot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("operation-log.json");
    let mut log = OperationLog::new(2);
    log.push(sample_operation(
        ControlAction::Unfreeze,
        OperationResult::Success,
    ));

    log.persist_json(&path).expect("persist log");
    let persisted = OperationLog::load_persisted_json(&path)
        .expect("read log")
        .expect("log exists");

    assert!(persisted.contains("\"action\":\"unfreeze\""));
}

#[test]
fn manager_v2_reports_health_capabilities_and_self_check() {
    let health = ModuleHealth::evaluate(true, true, false, true, true, true);
    let capabilities = vec![ControlCapability::missing(
        CapabilityName::LsposedSystemServer,
        "hook missing",
    )];

    assert!(health_report_json(&health).contains("\"status\":\"degraded\""));
    assert!(capability_report_json(&capabilities).contains("\"status\":\"missing\""));
    assert!(self_check_json(&health, &capabilities).contains("\"controlAllowed\":false"));
}

fn sample_operation(action: ControlAction, result: OperationResult) -> ControlOperation {
    ControlOperation {
        operation_id: 7,
        timestamp_ms: 42,
        package_name: "com.example.app".to_owned(),
        uid: 10_123,
        pid_list: vec![123, 124],
        action,
        backend: "cgroup-v2".to_owned(),
        reason: "delay elapsed".to_owned(),
        result,
        details: "all processes updated".to_owned(),
    }
}
