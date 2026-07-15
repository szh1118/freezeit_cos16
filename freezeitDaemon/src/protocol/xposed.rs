use crate::app::error::DaemonError;

pub const XPOSED_SOCKET_NAME: &str = "\0FreezeitXposedServer";
pub const FREEZEIT_COMMAND_BASE: i32 = 1_359_322_925;
pub const HEADER_LEN: usize = 8;
/// Legacy upper bound for raw bridge responses such as the Xposed log.
pub const MAX_PAYLOAD_LEN: usize = 1024 * 1024;
/// FreezeitService allocates a 128 KiB request buffer and rejects payloadLen
/// >= its length. Keep Rust's framed request limit one byte below it.
pub const MAX_REQUEST_PAYLOAD_LEN: usize = 128 * 1024 - 1;
const MAX_HEALTH_JSON_NESTING: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum XposedCommand {
    GetForeground = FREEZEIT_COMMAND_BASE + 1,
    GetScreen = FREEZEIT_COMMAND_BASE + 2,
    GetXpLog = FREEZEIT_COMMAND_BASE + 3,
    SetConfig = FREEZEIT_COMMAND_BASE + 20,
    SetWakeupLock = FREEZEIT_COMMAND_BASE + 21,
    BreakNetwork = FREEZEIT_COMMAND_BASE + 41,
    UpdatePending = FREEZEIT_COMMAND_BASE + 60,
    GetHookHealth = FREEZEIT_COMMAND_BASE + 70,
    GetRuntimeAppStates = FREEZEIT_COMMAND_BASE + 71,
    GetSystemFreezerHints = FREEZEIT_COMMAND_BASE + 72,
}

impl TryFrom<i32> for XposedCommand {
    type Error = DaemonError;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            x if x == Self::GetForeground as i32 => Ok(Self::GetForeground),
            x if x == Self::GetScreen as i32 => Ok(Self::GetScreen),
            x if x == Self::GetXpLog as i32 => Ok(Self::GetXpLog),
            x if x == Self::SetConfig as i32 => Ok(Self::SetConfig),
            x if x == Self::SetWakeupLock as i32 => Ok(Self::SetWakeupLock),
            x if x == Self::BreakNetwork as i32 => Ok(Self::BreakNetwork),
            x if x == Self::UpdatePending as i32 => Ok(Self::UpdatePending),
            x if x == Self::GetHookHealth as i32 => Ok(Self::GetHookHealth),
            x if x == Self::GetRuntimeAppStates as i32 => Ok(Self::GetRuntimeAppStates),
            x if x == Self::GetSystemFreezerHints as i32 => Ok(Self::GetSystemFreezerHints),
            _ => Err(DaemonError::protocol(format!(
                "unknown xposed command {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XposedFrame {
    pub command: XposedCommand,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookBridgeStatus {
    Active,
    Missing(String),
    Degraded(String),
}

impl HookBridgeStatus {
    pub fn is_ready_for_control(&self) -> bool {
        matches!(self, Self::Active)
    }

    pub fn health_label(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Missing(_) => "missing",
            Self::Degraded(_) => "degraded",
        }
    }
}

pub fn classify_bridge_error(error: &DaemonError) -> HookBridgeStatus {
    let message = error.to_string();
    if message.contains("No such file")
        || message.contains("Connection refused")
        || message.contains("not found")
    {
        HookBridgeStatus::Missing(message)
    } else {
        HookBridgeStatus::Degraded(message)
    }
}

pub fn classify_hook_health_payload(payload: &str) -> HookBridgeStatus {
    // Modern hook reports intentionally keep the aggregate `status` degraded
    // when optional scopes are unavailable. The control-relevant value is the
    // explicit top-level system_control_status. Older hooks only expose status.
    let status = match top_level_json_string_field(payload, "system_control_status") {
        Ok(Some(status)) => Some(status),
        Ok(None) => match top_level_json_string_field(payload, "status") {
            Ok(status) => status,
            Err(()) => {
                return HookBridgeStatus::Degraded(format!(
                    "unrecognized hook health payload: {payload}"
                ));
            }
        },
        Err(()) => {
            return HookBridgeStatus::Degraded(format!(
                "unrecognized hook health payload: {payload}"
            ));
        }
    };

    match status.as_deref() {
        Some("active") => HookBridgeStatus::Active,
        Some("degraded") | Some("inactive") | Some("missing") => {
            HookBridgeStatus::Degraded(payload.to_owned())
        }
        _ => HookBridgeStatus::Degraded(format!("unrecognized hook health payload: {payload}")),
    }
}

fn top_level_json_string_field(payload: &str, field: &str) -> Result<Option<String>, ()> {
    let bytes = payload.as_bytes();
    let mut index = 0;
    skip_json_whitespace(bytes, &mut index);
    if bytes.get(index) != Some(&b'{') {
        return Err(());
    }
    index += 1;
    let mut value = None;

    loop {
        skip_json_whitespace(bytes, &mut index);
        if bytes.get(index) == Some(&b'}') {
            index += 1;
            skip_json_whitespace(bytes, &mut index);
            return (index == bytes.len()).then_some(value).ok_or(());
        }

        let key = parse_json_string(bytes, &mut index)?;
        skip_json_whitespace(bytes, &mut index);
        if bytes.get(index) != Some(&b':') {
            return Err(());
        }
        index += 1;
        skip_json_whitespace(bytes, &mut index);
        if key == field {
            value = Some(parse_json_string(bytes, &mut index)?);
        } else {
            skip_json_value(bytes, &mut index, 0)?;
        }

        skip_json_whitespace(bytes, &mut index);
        match bytes.get(index) {
            Some(b',') => index += 1,
            Some(b'}') => {
                index += 1;
                skip_json_whitespace(bytes, &mut index);
                return (index == bytes.len()).then_some(value).ok_or(());
            }
            _ => return Err(()),
        }
    }
}

fn skip_json_value(bytes: &[u8], index: &mut usize, depth: usize) -> Result<(), ()> {
    skip_json_whitespace(bytes, index);
    match bytes.get(*index).copied() {
        Some(b'"') => {
            let _ = parse_json_string(bytes, index)?;
            Ok(())
        }
        Some(b'{') => {
            if depth >= MAX_HEALTH_JSON_NESTING {
                return Err(());
            }
            *index += 1;
            skip_json_whitespace(bytes, index);
            if bytes.get(*index) == Some(&b'}') {
                *index += 1;
                return Ok(());
            }
            loop {
                let _ = parse_json_string(bytes, index)?;
                skip_json_whitespace(bytes, index);
                if bytes.get(*index) != Some(&b':') {
                    return Err(());
                }
                *index += 1;
                skip_json_value(bytes, index, depth + 1)?;
                skip_json_whitespace(bytes, index);
                match bytes.get(*index) {
                    Some(b',') => *index += 1,
                    Some(b'}') => {
                        *index += 1;
                        return Ok(());
                    }
                    _ => return Err(()),
                }
                skip_json_whitespace(bytes, index);
            }
        }
        Some(b'[') => {
            if depth >= MAX_HEALTH_JSON_NESTING {
                return Err(());
            }
            *index += 1;
            skip_json_whitespace(bytes, index);
            if bytes.get(*index) == Some(&b']') {
                *index += 1;
                return Ok(());
            }
            loop {
                skip_json_value(bytes, index, depth + 1)?;
                skip_json_whitespace(bytes, index);
                match bytes.get(*index) {
                    Some(b',') => *index += 1,
                    Some(b']') => {
                        *index += 1;
                        return Ok(());
                    }
                    _ => return Err(()),
                }
                skip_json_whitespace(bytes, index);
            }
        }
        Some(b't') => consume_json_literal(bytes, index, b"true"),
        Some(b'f') => consume_json_literal(bytes, index, b"false"),
        Some(b'n') => consume_json_literal(bytes, index, b"null"),
        Some(b'-' | b'0'..=b'9') => {
            let start = *index;
            while matches!(
                bytes.get(*index),
                Some(b'0'..=b'9' | b'+' | b'-' | b'.' | b'e' | b'E')
            ) {
                *index += 1;
            }
            (start != *index).then_some(()).ok_or(())
        }
        _ => Err(()),
    }
}

fn consume_json_literal(bytes: &[u8], index: &mut usize, literal: &[u8]) -> Result<(), ()> {
    let end = index.checked_add(literal.len()).ok_or(())?;
    if bytes.get(*index..end) != Some(literal) {
        return Err(());
    }
    *index = end;
    Ok(())
}

fn parse_json_string(bytes: &[u8], index: &mut usize) -> Result<String, ()> {
    if bytes.get(*index) != Some(&b'"') {
        return Err(());
    }
    *index += 1;
    let mut value = String::new();
    while let Some(byte) = bytes.get(*index).copied() {
        *index += 1;
        match byte {
            b'"' => return Ok(value),
            b'\\' => {
                let escaped = bytes.get(*index).copied().ok_or(())?;
                *index += 1;
                match escaped {
                    b'"' | b'\\' | b'/' => value.push(escaped as char),
                    b'b' => value.push('\u{0008}'),
                    b'f' => value.push('\u{000c}'),
                    b'n' => value.push('\n'),
                    b'r' => value.push('\r'),
                    b't' => value.push('\t'),
                    b'u' => {
                        let end = index.checked_add(4).ok_or(())?;
                        let hex = bytes.get(*index..end).ok_or(())?;
                        if !hex.iter().all(u8::is_ascii_hexdigit) {
                            return Err(());
                        }
                        *index = end;
                        value.push('?');
                    }
                    _ => return Err(()),
                }
            }
            0..=0x1f => return Err(()),
            other => value.push(other as char),
        }
    }
    Err(())
}

fn skip_json_whitespace(bytes: &[u8], index: &mut usize) {
    while matches!(bytes.get(*index), Some(b' ' | b'\n' | b'\r' | b'\t')) {
        *index += 1;
    }
}

pub fn parse_foreground_uid_payload(payload: &[u8]) -> Result<Vec<u32>, DaemonError> {
    if payload.len() < 4 {
        return Err(DaemonError::protocol(
            "foreground uid payload count is missing",
        ));
    }

    let count = i32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
    if count < 0 {
        return Err(DaemonError::protocol(
            "foreground uid payload count is negative",
        ));
    }

    let count = count as usize;
    let expected_len = 4 + count * 4;
    if payload.len() < expected_len {
        return Err(DaemonError::protocol(format!(
            "foreground uid payload length mismatch: expected at least {expected_len}, got {}",
            payload.len()
        )));
    }

    Ok((0..count)
        .map(|index| {
            let offset = 4 + index * 4;
            i32::from_le_bytes([
                payload[offset],
                payload[offset + 1],
                payload[offset + 2],
                payload[offset + 3],
            ]) as u32
        })
        .collect())
}

pub fn parse_frame(bytes: &[u8]) -> Result<XposedFrame, DaemonError> {
    if bytes.len() < HEADER_LEN {
        return Err(DaemonError::protocol("xposed frame header is incomplete"));
    }

    let command = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let payload_len = i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if payload_len < 0 {
        return Err(DaemonError::protocol(
            "xposed frame payload length is negative",
        ));
    }

    let payload_len = payload_len as usize;
    if payload_len > MAX_REQUEST_PAYLOAD_LEN {
        return Err(DaemonError::protocol("xposed frame payload is too large"));
    }

    let expected_len = HEADER_LEN + payload_len;
    if bytes.len() != expected_len {
        return Err(DaemonError::protocol(format!(
            "xposed frame length mismatch: expected {expected_len}, got {}",
            bytes.len()
        )));
    }

    Ok(XposedFrame {
        command: XposedCommand::try_from(command)?,
        payload: bytes[HEADER_LEN..].to_vec(),
    })
}

pub fn encode_frame(command: XposedCommand, payload: &[u8]) -> Result<Vec<u8>, DaemonError> {
    if payload.len() > MAX_REQUEST_PAYLOAD_LEN {
        return Err(DaemonError::protocol("xposed frame payload is too large"));
    }

    let mut bytes = Vec::with_capacity(HEADER_LEN + payload.len());
    bytes.extend_from_slice(&(command as i32).to_le_bytes());
    bytes.extend_from_slice(&(payload.len() as i32).to_le_bytes());
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_hook_status_is_not_overridden_by_nested_active_hook_status() {
        let payload = r#"{"status":"degraded","system_control_status":"degraded","hooks":[{"status":"active"}]}"#;

        assert!(matches!(
            classify_hook_health_payload(payload),
            HookBridgeStatus::Degraded(_)
        ));
    }

    #[test]
    fn system_control_status_takes_priority_over_aggregate_status() {
        let payload = r#"{"status":"degraded","system_control_status":"active","hooks":[{"status":"degraded"}]}"#;

        assert_eq!(
            classify_hook_health_payload(payload),
            HookBridgeStatus::Active
        );
    }

    #[test]
    fn excessively_nested_hook_health_payload_fails_closed() {
        let nesting = 80;
        let payload = format!(
            "{{\"system_control_status\":\"active\",\"nested\":{}0{}}}",
            "[".repeat(nesting),
            "]".repeat(nesting)
        );

        assert!(matches!(
            classify_hook_health_payload(&payload),
            HookBridgeStatus::Degraded(_)
        ));
    }

    #[test]
    fn rejects_frames_that_the_java_hook_buffer_cannot_accept() {
        let payload = vec![0_u8; 128 * 1024];

        assert!(encode_frame(XposedCommand::SetConfig, &payload).is_err());
    }

    #[test]
    fn request_limit_does_not_shrink_the_legacy_bridge_response_budget() {
        assert_eq!(MAX_REQUEST_PAYLOAD_LEN, 128 * 1024 - 1);
        assert_eq!(MAX_PAYLOAD_LEN, 1024 * 1024);
    }
}
