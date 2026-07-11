use freezeit_daemon::app::logging::{decode_log_view, LogLevel, LogRecord, LogView, ManagerLog};
use freezeit_daemon::domain::operation::{ControlAction, ControlOperation, OperationResult};

#[test]
fn legacy_debug_values_migrate_without_touching_storage() {
    assert_eq!(decode_log_view(0), (LogView::Info, false));
    assert_eq!(decode_log_view(1), (LogView::Debug, false));
    assert_eq!(decode_log_view(2), (LogView::Warn, false));
    assert_eq!(decode_log_view(3), (LogView::Error, false));
    assert_eq!(decode_log_view(4), (LogView::Critical, false));
    assert_eq!(decode_log_view(255), (LogView::Info, true));
}

#[test]
fn info_is_legacy_only_and_debug_contains_every_category() {
    let mut log = ManagerLog::new(16);
    log.push(LogRecord::legacy_text(1_000, "⚙️设置成功"));
    log.push(LogRecord::diagnostic(
        LogLevel::Debug,
        2_000,
        "daemon active: apps=2",
    ));
    log.push(LogRecord::fault(LogLevel::Warn, 3_000, "hook degraded"));

    assert_eq!(log.render(LogView::Info), "[08:00:01]  ⚙️设置成功\n");
    assert!(!log.render(LogView::Info).contains("daemon active:"));
    assert!(!log.render(LogView::Info).contains("hook degraded"));

    let debug = log.render(LogView::Debug);
    assert!(debug.contains("⚙️设置成功"));
    assert!(debug.contains("[DEBUG] daemon active: apps=2"));
    assert!(debug.contains("[WARN] hook degraded"));
}

#[test]
fn severity_views_follow_the_approved_matrix() {
    let mut log = ManagerLog::new(16);
    for level in [
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Error,
        LogLevel::Critical,
        LogLevel::Debug,
    ] {
        log.push(LogRecord::diagnostic(level, 1_000, format!("{level:?}")));
    }

    assert_eq!(log.render(LogView::Warn).lines().count(), 3);
    assert_eq!(log.render(LogView::Error).lines().count(), 2);
    assert_eq!(log.render(LogView::Critical).lines().count(), 1);
    assert_eq!(log.render(LogView::Debug).lines().count(), 5);
}

#[test]
fn bounded_log_discards_oldest_record_and_clear_removes_all_views() {
    let mut log = ManagerLog::new(2);
    log.push(LogRecord::legacy_text(1_000, "first"));
    log.push(LogRecord::legacy_text(2_000, "second"));
    log.push(LogRecord::legacy_text(3_000, "third"));

    let info = log.render(LogView::Info);
    assert!(!info.contains("first"));
    assert!(info.contains("second"));
    assert!(info.contains("third"));

    log.clear();
    assert!(log.is_empty());
    assert_eq!(log.render(LogView::Info), "");
    assert_eq!(log.render(LogView::Debug), "");
}

#[test]
fn duplicate_runtime_operation_is_rendered_once() {
    let operation = ControlOperation {
        operation_id: 7,
        timestamp_ms: 42,
        package_name: "com.example.app".to_owned(),
        uid: 10_123,
        pid_list: vec![123, 124],
        action: ControlAction::Freeze,
        backend: "cgroup-v2".to_owned(),
        reason: "delay elapsed".to_owned(),
        result: OperationResult::Success,
        details: "all processes updated".to_owned(),
    };
    let mut log = ManagerLog::new(8);

    log.push(LogRecord::operation(operation.clone()));
    log.push(LogRecord::operation(operation));

    assert_eq!(log.render(LogView::Info).lines().count(), 1);
}

#[test]
fn cleared_or_evicted_operations_are_not_replayed_from_runtime_ring() {
    let operation = ControlOperation {
        operation_id: 7,
        timestamp_ms: 42,
        package_name: "com.example.app".to_owned(),
        uid: 10_123,
        pid_list: vec![123],
        action: ControlAction::Freeze,
        backend: "cgroup-v2".to_owned(),
        reason: "delay elapsed".to_owned(),
        result: OperationResult::Success,
        details: String::new(),
    };
    let mut log = ManagerLog::new(1);

    log.push(LogRecord::operation(operation.clone()));
    log.push(LogRecord::legacy_text(1_000, "⚙️设置成功"));
    log.push(LogRecord::operation(operation.clone()));
    assert_eq!(log.render(LogView::Info), "[08:00:01]  ⚙️设置成功\n");

    log.clear();
    log.push(LogRecord::operation(operation));
    assert_eq!(log.render(LogView::Info), "");
}

#[test]
fn info_operation_matches_cpp_manager_style_and_debug_keeps_diagnostics() {
    let operation = ControlOperation {
        operation_id: 7,
        timestamp_ms: 42,
        package_name: "com.example.app".to_owned(),
        uid: 10_123,
        pid_list: vec![123, 124],
        action: ControlAction::Freeze,
        backend: "cgroup-v2".to_owned(),
        reason: "delay elapsed".to_owned(),
        result: OperationResult::Success,
        details: "all processes updated".to_owned(),
    };
    let mut log = ManagerLog::new(8);
    log.push(LogRecord::operation(operation));

    assert_eq!(
        log.render(LogView::Info),
        "[08:00:00]  ❄️冻结 com.example.app 2进程\n"
    );

    let debug = log.render(LogView::Debug);
    assert!(debug.contains("❄️冻结 com.example.app 2进程"));
    assert!(debug.contains("UID:10123"));
    assert!(debug.contains("PID:123,124"));
    assert!(debug.contains("方式:cgroup-v2"));
    assert!(debug.contains("结果:成功"));
    assert!(debug.contains("原因:delay elapsed"));
}

#[test]
fn info_omits_rust_only_and_failed_operations_but_debug_keeps_them() {
    let mut log = ManagerLog::new(8);
    for (operation_id, action, result, reason) in [
        (
            1,
            ControlAction::Fallback,
            OperationResult::Failed,
            "fallback failed",
        ),
        (
            2,
            ControlAction::Skip,
            OperationResult::Skipped,
            "policy skipped",
        ),
        (
            3,
            ControlAction::Recover,
            OperationResult::Success,
            "restart reconciliation",
        ),
        (
            4,
            ControlAction::Postpone,
            OperationResult::Postponed,
            "pending freeze delay 10000ms",
        ),
        (
            5,
            ControlAction::Freeze,
            OperationResult::Failed,
            "control failed",
        ),
    ] {
        log.push(LogRecord::operation(ControlOperation {
            operation_id,
            timestamp_ms: 42,
            package_name: "com.example.app".to_owned(),
            uid: 10_123,
            pid_list: vec![123],
            action,
            backend: "cgroup-v2".to_owned(),
            reason: reason.to_owned(),
            result,
            details: String::new(),
        }));
    }

    assert_eq!(log.render(LogView::Info), "");

    let debug = log.render(LogView::Debug);
    assert!(debug.contains("⚠️降级处理"));
    assert!(debug.contains("⚠️跳过"));
    assert!(debug.contains("♻️恢复"));
    assert!(debug.contains("⏳延迟冻结"));
    assert!(debug.contains("结果:失败"));
}

#[test]
fn only_verified_legacy_lines_can_bypass_timestamp_rendering() {
    let record =
        LogRecord::verified_legacy_line("[12:34:56]  ⚙️设置成功").expect("valid legacy line");
    let mut log = ManagerLog::new(4);
    log.push(record);

    assert_eq!(log.render(LogView::Info), "[12:34:56]  ⚙️设置成功\n");
    assert!(LogRecord::verified_legacy_line("daemon active: apps=1").is_none());
    assert!(LogRecord::verified_legacy_line("[2026-07-11] loaded config").is_none());
    assert!(LogRecord::verified_legacy_line("[12:34:56]  daemon active: apps=1").is_none());
    assert!(LogRecord::verified_legacy_line(
        "[12:34:56]  com.example.app:123 Binder正在传输, 延迟后再冻结"
    )
    .is_some());
}
