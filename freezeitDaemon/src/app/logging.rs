use std::{
    collections::VecDeque,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    app::operation_log::{
        legacy_timestamped_line, operation_is_legacy_info, operation_to_debug_text,
        operation_to_legacy_text,
    },
    domain::operation::{ControlOperation, OperationResult},
};

pub const LOG_LEVEL_SETTING_INDEX: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogView {
    Info,
    Warn,
    Error,
    Critical,
    Debug,
}

pub fn decode_log_view(value: u8) -> (LogView, bool) {
    match value {
        0 => (LogView::Info, false),
        1 => (LogView::Debug, false),
        2 => (LogView::Warn, false),
        3 => (LogView::Error, false),
        4 => (LogView::Critical, false),
        _ => (LogView::Info, true),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
    Critical,
    Debug,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogCategory {
    LegacyOperation,
    Diagnostic,
    Fault,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogPayload {
    Text(String),
    VerifiedLegacyLine(String),
    Operation(ControlOperation),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    pub level: LogLevel,
    pub category: LogCategory,
    pub timestamp_ms: u128,
    pub payload: LogPayload,
}

impl LogRecord {
    pub fn new(level: LogLevel, message: impl Into<String>) -> Self {
        Self::diagnostic(level, now_ms(), message)
    }

    pub fn legacy_text(timestamp_ms: u128, message: impl Into<String>) -> Self {
        Self {
            level: LogLevel::Info,
            category: LogCategory::LegacyOperation,
            timestamp_ms,
            payload: LogPayload::Text(message.into()),
        }
    }

    pub fn diagnostic(level: LogLevel, timestamp_ms: u128, message: impl Into<String>) -> Self {
        Self {
            level,
            category: LogCategory::Diagnostic,
            timestamp_ms,
            payload: LogPayload::Text(message.into()),
        }
    }

    pub fn fault(level: LogLevel, timestamp_ms: u128, message: impl Into<String>) -> Self {
        Self {
            level,
            category: LogCategory::Fault,
            timestamp_ms,
            payload: LogPayload::Text(message.into()),
        }
    }

    pub fn operation(operation: ControlOperation) -> Self {
        Self {
            level: operation_log_level(&operation),
            category: LogCategory::LegacyOperation,
            timestamp_ms: operation.timestamp_ms,
            payload: LogPayload::Operation(operation),
        }
    }

    pub fn verified_legacy_line(line: impl Into<String>) -> Option<Self> {
        let line = line.into();
        if !is_verified_legacy_line(&line) {
            return None;
        }
        Some(Self {
            level: LogLevel::Info,
            category: LogCategory::LegacyOperation,
            timestamp_ms: 0,
            payload: LogPayload::VerifiedLegacyLine(line),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerLog {
    capacity: usize,
    records: VecDeque<LogRecord>,
    highest_operation_id: u64,
    operation_timestamp_cutoff_ms: Option<u128>,
    content_version: u64,
}

impl ManagerLog {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            records: VecDeque::new(),
            highest_operation_id: 0,
            operation_timestamp_cutoff_ms: None,
            content_version: 0,
        }
    }

    pub fn push(&mut self, record: LogRecord) {
        if let LogPayload::Operation(operation) = &record.payload {
            if self
                .operation_timestamp_cutoff_ms
                .is_some_and(|cutoff| operation.timestamp_ms <= cutoff)
            {
                return;
            }
            if operation.operation_id != 0 {
                if operation.operation_id <= self.highest_operation_id {
                    return;
                }
                self.highest_operation_id = operation.operation_id;
            } else {
                let duplicate = self.records.iter().any(|existing| {
                    matches!(
                        &existing.payload,
                        LogPayload::Operation(existing_operation)
                            if existing_operation == operation
                    )
                });
                if duplicate {
                    return;
                }
            }
        }

        if self.records.len() == self.capacity {
            self.records.pop_front();
        }
        self.records.push_back(record);
        self.bump_content_version();
    }

    pub fn push_once(&mut self, record: LogRecord) {
        let duplicate = self.records.iter().any(|existing| {
            existing.level == record.level
                && existing.category == record.category
                && existing.payload == record.payload
        });
        if !duplicate {
            self.push(record);
        }
    }

    pub fn clear(&mut self) {
        self.operation_timestamp_cutoff_ms = Some(now_ms());
        let had_records = !self.records.is_empty();
        self.records.clear();
        if had_records {
            self.bump_content_version();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Monotonically advances whenever the rendered record buffer changes.
    ///
    /// The legacy manager protocol currently compares only response lengths, so callers that
    /// can negotiate a version-aware response can use this to detect same-length replacements.
    pub fn content_version(&self) -> u64 {
        self.content_version
    }

    pub fn render(&self, view: LogView) -> String {
        self.records
            .iter()
            .filter(|record| record_is_visible(record, view))
            .map(|record| render_record(record, view))
            .collect()
    }

    fn bump_content_version(&mut self) {
        self.content_version = self.content_version.saturating_add(1);
    }
}

impl Default for ManagerLog {
    fn default() -> Self {
        Self::new(256)
    }
}

fn record_is_visible(record: &LogRecord, view: LogView) -> bool {
    match view {
        LogView::Info => {
            record.category == LogCategory::LegacyOperation
                && match &record.payload {
                    LogPayload::Operation(operation) => operation_is_legacy_info(operation),
                    _ => true,
                }
        }
        LogView::Warn => matches!(
            record.level,
            LogLevel::Warn | LogLevel::Error | LogLevel::Critical
        ),
        LogView::Error => matches!(record.level, LogLevel::Error | LogLevel::Critical),
        LogView::Critical => record.level == LogLevel::Critical,
        LogView::Debug => true,
    }
}

fn operation_log_level(operation: &ControlOperation) -> LogLevel {
    match operation.result {
        OperationResult::Partial => LogLevel::Warn,
        OperationResult::Failed => LogLevel::Error,
        OperationResult::Success | OperationResult::Skipped | OperationResult::Postponed => {
            LogLevel::Info
        }
    }
}

fn render_record(record: &LogRecord, view: LogView) -> String {
    match &record.payload {
        LogPayload::Operation(operation) if view == LogView::Debug => {
            operation_to_debug_text(operation)
        }
        LogPayload::Operation(operation) => operation_to_legacy_text(operation),
        LogPayload::VerifiedLegacyLine(line) => {
            let mut rendered = line.clone();
            if !rendered.ends_with('\n') {
                rendered.push('\n');
            }
            rendered
        }
        LogPayload::Text(message)
            if record.category == LogCategory::LegacyOperation
                && matches!(view, LogView::Info | LogView::Debug) =>
        {
            legacy_timestamped_line(record.timestamp_ms, message)
        }
        LogPayload::Text(message) => legacy_timestamped_line(
            record.timestamp_ms,
            &format!("[{}] {message}", level_name(record.level)),
        ),
    }
}

fn is_verified_legacy_line(line: &str) -> bool {
    let bytes = line.as_bytes();
    let has_timestamp = bytes.len() >= 12
        && bytes[0] == b'['
        && bytes[3] == b':'
        && bytes[6] == b':'
        && bytes[9] == b']'
        && bytes[10] == b' '
        && bytes[11] == b' '
        && [1, 2, 4, 5, 7, 8]
            .into_iter()
            .all(|index| bytes[index].is_ascii_digit());
    if !has_timestamp {
        return false;
    }
    let message = &line[12..];
    [
        "❄️冻结 ",
        "🧊冻结 ",
        "☀️解冻 ",
        "😁启动 ",
        "😭关闭 ",
        "⚙️设置成功",
    ]
    .iter()
    .any(|prefix| message.starts_with(prefix))
        || message.ends_with(" Binder正在传输, 延迟后再冻结")
}

fn level_name(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Info => "INFO",
        LogLevel::Warn => "WARN",
        LogLevel::Error => "ERROR",
        LogLevel::Critical => "CRITICAL",
        LogLevel::Debug => "DEBUG",
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

pub fn startup_message() -> LogRecord {
    LogRecord::diagnostic(LogLevel::Debug, now_ms(), "freezeit daemon starting")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::operation::{ControlAction, OperationResult};

    fn operation(
        operation_id: u64,
        timestamp_ms: u128,
        package_name: &str,
        result: OperationResult,
    ) -> ControlOperation {
        ControlOperation {
            operation_id,
            timestamp_ms,
            package_name: package_name.to_owned(),
            uid: 10_123,
            pid_list: vec![123],
            action: ControlAction::Freeze,
            backend: "cgroup.freeze".to_owned(),
            reason: "control pass".to_owned(),
            result,
            details: String::new(),
        }
    }

    #[test]
    fn clear_rejects_operation_stamped_before_the_clear_boundary() {
        let stale_timestamp = now_ms().saturating_sub(1);
        let mut log = ManagerLog::new(8);
        log.push(LogRecord::operation(operation(
            1,
            stale_timestamp,
            "com.example.before",
            OperationResult::Success,
        )));

        log.clear();
        log.push(LogRecord::operation(operation(
            2,
            stale_timestamp,
            "com.example.stale",
            OperationResult::Success,
        )));

        assert!(log.is_empty());
    }

    #[test]
    fn clear_accepts_operation_stamped_after_the_clear_boundary() {
        let mut log = ManagerLog::new(8);
        log.clear();
        let fresh_timestamp = log
            .operation_timestamp_cutoff_ms
            .expect("clear records an operation cutoff")
            .saturating_add(1);

        log.push(LogRecord::operation(operation(
            1,
            fresh_timestamp,
            "com.example.fresh",
            OperationResult::Success,
        )));

        assert!(log.render(LogView::Info).contains("com.example.fresh"));
    }

    #[test]
    fn content_version_advances_for_equal_length_replacement() {
        let mut log = ManagerLog::new(1);
        log.push(LogRecord::legacy_text(1_000, "first"));
        let first_length = log.render(LogView::Info).len();
        let first_version = log.content_version();

        log.push(LogRecord::legacy_text(2_000, "other"));

        assert_eq!(log.render(LogView::Info).len(), first_length);
        assert!(log.content_version() > first_version);
    }

    #[test]
    fn partial_and_failed_operations_are_visible_in_severity_views() {
        let mut log = ManagerLog::new(8);
        log.push(LogRecord::operation(operation(
            1,
            1_000,
            "com.example.partial",
            OperationResult::Partial,
        )));
        log.push(LogRecord::operation(operation(
            2,
            2_000,
            "com.example.failed",
            OperationResult::Failed,
        )));

        let warnings = log.render(LogView::Warn);
        assert!(warnings.contains("com.example.partial"));
        assert!(warnings.contains("com.example.failed"));
        assert!(!log.render(LogView::Error).contains("com.example.partial"));
        assert!(log.render(LogView::Error).contains("com.example.failed"));
        assert!(!log.render(LogView::Critical).contains("com.example.failed"));
    }
}
