use std::{collections::VecDeque, fs, path::Path};

use crate::app::error::DaemonError;
use crate::domain::operation::{ControlAction, ControlOperation, OperationResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationLog {
    capacity: usize,
    records: VecDeque<ControlOperation>,
}

impl OperationLog {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            records: VecDeque::new(),
        }
    }

    pub fn push(&mut self, operation: ControlOperation) {
        if self.records.len() == self.capacity {
            self.records.pop_front();
        }
        self.records.push_back(operation);
    }

    pub fn records(&self) -> impl Iterator<Item = &ControlOperation> {
        self.records.iter()
    }

    pub fn to_json(&self) -> String {
        let records = self
            .records
            .iter()
            .map(operation_to_json)
            .collect::<Vec<_>>()
            .join(",");
        format!("{{\"operations\":[{records}]}}")
    }

    pub fn to_legacy_text(&self) -> String {
        self.records
            .iter()
            .map(operation_to_legacy_text)
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn persist_json(&self, path: impl AsRef<Path>) -> Result<(), DaemonError> {
        fs::write(path, self.to_json()).map_err(DaemonError::from)
    }

    pub fn load_persisted_json(path: impl AsRef<Path>) -> Result<Option<String>, DaemonError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(None);
        }

        Ok(Some(fs::read_to_string(path)?))
    }
}

pub fn operation_to_json(operation: &ControlOperation) -> String {
    let pids = operation
        .pid_list
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"operationId\":{},\"timestampMs\":{},\"packageName\":\"{}\",\"uid\":{},\"pidList\":[{}],\"action\":\"{}\",\"backend\":\"{}\",\"reason\":\"{}\",\"result\":\"{}\",\"details\":\"{}\"}}",
        operation.operation_id,
        operation.timestamp_ms,
        escape_json(&operation.package_name),
        operation.uid,
        pids,
        action_name(operation.action),
        escape_json(&operation.backend),
        escape_json(&operation.reason),
        result_name(operation.result),
        escape_json(&operation.details)
    )
}

pub fn operation_to_legacy_text(operation: &ControlOperation) -> String {
    legacy_timestamped_line(operation.timestamp_ms, &legacy_operation_message(operation))
}

pub fn operation_to_debug_text(operation: &ControlOperation) -> String {
    legacy_timestamped_line(
        operation.timestamp_ms,
        &diagnostic_operation_message(operation),
    )
}

pub fn operation_is_legacy_info(operation: &ControlOperation) -> bool {
    match (operation.action, operation.result) {
        (ControlAction::Freeze, OperationResult::Success)
        | (ControlAction::Unfreeze, OperationResult::Success)
        | (ControlAction::Terminate, OperationResult::Success) => true,
        (ControlAction::Postpone, OperationResult::Postponed) => is_binder_blocker(operation),
        _ => false,
    }
}

pub fn legacy_timestamped_line(timestamp_ms: u128, message: &str) -> String {
    let mut line = format!("{}{}", legacy_time_prefix(timestamp_ms), message);
    if !line.ends_with('\n') {
        line.push('\n');
    }
    line
}

fn legacy_operation_message(operation: &ControlOperation) -> String {
    let package_name = sanitize_legacy_field(&operation.package_name);
    let process_text = legacy_process_text(operation.pid_list.len());

    match operation.action {
        ControlAction::Freeze => format!(
            "{} {package_name} {process_text}",
            legacy_freeze_phrase(operation)
        ),
        ControlAction::Unfreeze if operation.pid_list.is_empty() => {
            format!("😁启动 {package_name}")
        }
        ControlAction::Unfreeze => format!("☀️解冻 {package_name} {process_text}"),
        ControlAction::Terminate if operation.pid_list.is_empty() => {
            format!("😭关闭 {package_name}")
        }
        ControlAction::Terminate => format!("😭关闭 {package_name} {process_text}"),
        ControlAction::Postpone if is_binder_blocker(operation) => {
            let blocker_pid = operation
                .pid_list
                .first()
                .map(i32::to_string)
                .unwrap_or_else(|| "?".to_owned());
            format!("{package_name}:{blocker_pid} Binder正在传输, 延迟后再冻结")
        }
        ControlAction::Postpone => format!("⏳延迟冻结 {package_name} {process_text}"),
        ControlAction::Fallback => format!("⚠️降级处理 {package_name} {process_text}"),
        ControlAction::Skip => format!("⚠️跳过 {package_name} {process_text}"),
        ControlAction::Recover => format!("♻️恢复 {package_name} {process_text}"),
    }
}

fn diagnostic_operation_message(operation: &ControlOperation) -> String {
    let pids = operation
        .pid_list
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let package_name = sanitize_legacy_field(&operation.package_name);
    let process_text = legacy_process_text(operation.pid_list.len());
    let context = legacy_context_suffix(operation, &pids);

    match operation.action {
        ControlAction::Freeze => format!(
            "{} {package_name} {process_text}{context}",
            legacy_freeze_phrase(operation)
        ),
        ControlAction::Unfreeze if operation.pid_list.is_empty() => {
            format!("😁启动 {package_name}{context}")
        }
        ControlAction::Unfreeze => format!("☀️解冻 {package_name} {process_text}{context}"),
        ControlAction::Terminate if operation.pid_list.is_empty() => {
            format!("😭关闭 {package_name}{context}")
        }
        ControlAction::Terminate => format!("😭关闭 {package_name} {process_text}{context}"),
        ControlAction::Postpone if is_binder_blocker(operation) => {
            let blocker_pid = operation
                .pid_list
                .first()
                .map(i32::to_string)
                .unwrap_or_else(|| "?".to_owned());
            format!("{package_name}:{blocker_pid} Binder正在传输, 延迟后再冻结{context}")
        }
        ControlAction::Postpone => format!("⏳延迟冻结 {package_name} {process_text}{context}"),
        ControlAction::Fallback => format!("⚠️降级处理 {package_name} {process_text}{context}"),
        ControlAction::Skip => format!("⚠️跳过 {package_name} {process_text}{context}"),
        ControlAction::Recover => format!("♻️恢复 {package_name} {process_text}{context}"),
    }
}

fn legacy_process_text(process_count: usize) -> String {
    format!("{process_count}进程")
}

fn legacy_context_suffix(operation: &ControlOperation, pids: &str) -> String {
    let mut parts = vec![format!("UID:{}", operation.uid)];
    if !pids.is_empty() {
        parts.push(format!("PID:{pids}"));
    }
    if !operation.backend.trim().is_empty() {
        parts.push(format!(
            "方式:{}",
            sanitize_legacy_field(&operation.backend)
        ));
    }
    parts.push(format!("结果:{}", legacy_result_text(operation.result)));
    if !operation.reason.trim().is_empty() {
        parts.push(format!("原因:{}", sanitize_legacy_field(&operation.reason)));
    }
    if let Some(details) = meaningful_details(&operation.details) {
        parts.push(format!("详情:{}", sanitize_legacy_field(details)));
    }
    format!(" {}", parts.join(" "))
}

fn legacy_time_prefix(timestamp_ms: u128) -> String {
    let seconds = ((timestamp_ms / 1000) + 8 * 3600) % 86_400;
    let hour = seconds / 3600;
    let minute = (seconds % 3600) / 60;
    let second = seconds % 60;
    format!("[{hour:02}:{minute:02}:{second:02}]  ")
}

fn legacy_freeze_phrase(operation: &ControlOperation) -> &'static str {
    if operation.backend.contains("signal") {
        "🧊冻结"
    } else {
        "❄️冻结"
    }
}

fn legacy_result_text(result: OperationResult) -> &'static str {
    match result {
        OperationResult::Success => "成功",
        OperationResult::Partial => "部分完成",
        OperationResult::Failed => "失败",
        OperationResult::Skipped => "已跳过",
        OperationResult::Postponed => "已延迟",
    }
}

fn meaningful_details(details: &str) -> Option<&str> {
    let details = details.trim();
    if details.is_empty() {
        return None;
    }

    let details = if let Some(remainder) = details.strip_prefix("process_count=") {
        match remainder.find(char::is_whitespace) {
            Some(separator) => remainder[separator..].trim_start(),
            None => "",
        }
    } else {
        details
    };

    (!details.is_empty()).then_some(details)
}

fn is_binder_blocker(operation: &ControlOperation) -> bool {
    let mut evidence = String::with_capacity(operation.reason.len() + operation.details.len() + 1);
    evidence.push_str(&operation.reason);
    evidence.push(' ');
    evidence.push_str(&operation.details);
    evidence.to_ascii_lowercase().contains("binder")
}

fn sanitize_legacy_field(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '\n' | '\r' | '\t' => ' ',
            other => other,
        })
        .collect()
}

fn action_name(action: ControlAction) -> &'static str {
    match action {
        ControlAction::Freeze => "freeze",
        ControlAction::Unfreeze => "unfreeze",
        ControlAction::Terminate => "terminate",
        ControlAction::Postpone => "postpone",
        ControlAction::Fallback => "fallback",
        ControlAction::Skip => "skip",
        ControlAction::Recover => "recover",
    }
}

fn result_name(result: OperationResult) -> &'static str {
    match result {
        OperationResult::Success => "success",
        OperationResult::Partial => "partial",
        OperationResult::Failed => "failed",
        OperationResult::Skipped => "skipped",
        OperationResult::Postponed => "postponed",
    }
}

fn escape_json(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| match character {
            '"' => "\\\"".chars().collect::<Vec<_>>(),
            '\\' => "\\\\".chars().collect(),
            '\n' => "\\n".chars().collect(),
            '\r' => "\\r".chars().collect(),
            '\t' => "\\t".chars().collect(),
            // RFC 8259 要求 U+0000..U+001F 用 \uXXXX 转义，否则裸控制字符（NUL、ESC
            // 等）会让 manager 端 JSON parser 拒绝整条 operation log，丢失全部历史。
            c if (c as u32) < 0x20 => format!("\\u{:04x}", c as u32).chars().collect::<Vec<_>>(),
            other => vec![other],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_text_keeps_evidence_after_process_count_prefix() {
        let operation = ControlOperation {
            operation_id: 7,
            timestamp_ms: 1_000,
            package_name: "com.example.partial".to_owned(),
            uid: 10_123,
            pid_list: vec![123, 124],
            action: ControlAction::Freeze,
            backend: "signal".to_owned(),
            reason: "rollback required".to_owned(),
            result: OperationResult::Partial,
            details: "process_count=2 signaled=2 rolled_back=1 rollback_failures=1 binder=busy"
                .to_owned(),
        };

        let rendered = operation_to_debug_text(&operation);

        assert!(rendered.contains("详情:signaled=2 rolled_back=1 rollback_failures=1 binder=busy"));
        assert!(!rendered.contains("详情:process_count="));
    }
}
