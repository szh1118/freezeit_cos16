use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::app::{
    error::DaemonError,
    logging::{decode_log_view, LogRecord, LogView, ManagerLog, LOG_LEVEL_SETTING_INDEX},
};
use crate::config::migration::{
    parse_legacy_policy_line, parse_legacy_policy_target, LegacyPolicyTarget,
};

pub const MANAGER_LISTEN_HOST: &str = "127.0.0.1";
pub const MANAGER_LISTEN_PORT: u16 = 60613;
pub const HEADER_LEN: usize = 6;
pub const MAX_PAYLOAD_LEN: usize = 1024 * 1024;
const CPU_HISTORY_BUCKETS: usize = 32;
const CPU_CHART_CORES: usize = 8;

static REALTIME_SAMPLER: OnceLock<Mutex<RealtimeSampler>> = OnceLock::new();

#[cfg(test)]
thread_local! {
    static REALTIME_SAMPLE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ManagerCommand {
    GetPropInfo = 2,
    GetChangelog = 3,
    GetLog = 4,
    GetAppCfg = 5,
    GetRealTimeInfo = 6,
    GetSettings = 8,
    GetUidTime = 9,
    GetXpLog = 10,
    GetFreezeStatus = 11,
    SetAppCfg = 21,
    SetAppLabel = 22,
    SetSettingsVar = 23,
    ClearLog = 61,
    GetProcState = 62,
    GetHealthReport = 71,
    GetCapabilityReport = 72,
    GetCompatibilityBaseline = 73,
    GetOperationLogJson = 74,
    RunSelfCheck = 75,
}

impl TryFrom<u8> for ManagerCommand {
    type Error = DaemonError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            2 => Ok(Self::GetPropInfo),
            3 => Ok(Self::GetChangelog),
            4 => Ok(Self::GetLog),
            5 => Ok(Self::GetAppCfg),
            6 => Ok(Self::GetRealTimeInfo),
            8 => Ok(Self::GetSettings),
            9 => Ok(Self::GetUidTime),
            10 => Ok(Self::GetXpLog),
            11 => Ok(Self::GetFreezeStatus),
            21 => Ok(Self::SetAppCfg),
            22 => Ok(Self::SetAppLabel),
            23 => Ok(Self::SetSettingsVar),
            61 => Ok(Self::ClearLog),
            62 => Ok(Self::GetProcState),
            71 => Ok(Self::GetHealthReport),
            72 => Ok(Self::GetCapabilityReport),
            73 => Ok(Self::GetCompatibilityBaseline),
            74 => Ok(Self::GetOperationLogJson),
            75 => Ok(Self::RunSelfCheck),
            _ => Err(DaemonError::protocol(format!(
                "unknown manager command {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerFrame {
    pub command: ManagerCommand,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagerAppConfigRecord {
    pub uid: u32,
    pub mode: i32,
    pub permissive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagerFreezeStatusRecord {
    pub uid: u32,
    pub foreground: bool,
    pub state: i32,
    pub seconds: i32,
    pub process_count: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOnlyState {
    pub module_id: String,
    pub module_name: String,
    pub version: String,
    pub version_code: u32,
    pub author: String,
    pub cluster_num: u32,
    pub manager_log: ManagerLog,
    pub changelog: String,
    pub settings: Vec<u8>,
    pub xp_log: String,
    pub app_config: Vec<ManagerAppConfigRecord>,
    /// Generation changes whenever the effective persisted settings or app policy
    /// changes. Socket workers use it to reject stale hook-sync work.
    pub config_revision: u64,
    /// The original first token for each resolved app-config record. Keeping this
    /// lets persistence update the right line without treating discarded package
    /// rows as loaded records.
    pub app_config_source_tokens: BTreeMap<u32, String>,
    pub freeze_status: Vec<ManagerFreezeStatusRecord>,
    pub module_env: String,
    pub work_mode: String,
    pub android_version: String,
    pub kernel_version: String,
    pub ext_memory_mib: u32,
    pub daemon_health: String,
    pub hook_health: String,
    pub health_report_json: String,
    pub capability_report_json: String,
    pub compatibility_report_json: String,
    pub operation_log_json: String,
    pub self_check_json: String,
    pub control_allowed: bool,
    pub verified_targets: Vec<(String, u32)>,
    pub hook_config_synced: bool,
    pub settings_path: Option<String>,
    pub app_config_path: Option<String>,
    pub app_label_path: Option<String>,
    pub uid_time_path: String,
    pub uid_time_totals: BTreeMap<u32, i32>,
}

impl Default for ReadOnlyState {
    fn default() -> Self {
        Self {
            module_id: "freezeit".to_owned(),
            module_name: "Freezeit".to_owned(),
            version: "0.1.0-rust".to_owned(),
            version_code: 1,
            author: "jark006".to_owned(),
            cluster_num: 0,
            manager_log: ManagerLog::default(),
            changelog: String::new(),
            settings: legacy_default_settings(),
            xp_log: "hook health unknown\n".to_owned(),
            app_config: Vec::new(),
            config_revision: 0,
            app_config_source_tokens: BTreeMap::new(),
            freeze_status: Vec::new(),
            module_env: "Magisk".to_owned(),
            work_mode: "Rust daemon degraded".to_owned(),
            android_version: "Unknown".to_owned(),
            kernel_version: "Unknown".to_owned(),
            ext_memory_mib: 0,
            daemon_health: "degraded".to_owned(),
            hook_health: "unknown".to_owned(),
            health_report_json: "{\"status\":\"degraded\"}".to_owned(),
            capability_report_json: "{\"capabilities\":[]}".to_owned(),
            compatibility_report_json: "{\"capabilities\":[]}".to_owned(),
            operation_log_json: "{\"operations\":[]}".to_owned(),
            self_check_json: "{\"controlAllowed\":false}".to_owned(),
            control_allowed: false,
            verified_targets: Vec::new(),
            hook_config_synced: false,
            settings_path: None,
            app_config_path: None,
            app_label_path: None,
            uid_time_path: "/proc/uid_cputime/show_uid_stat".to_owned(),
            uid_time_totals: BTreeMap::new(),
        }
    }
}

pub fn legacy_default_settings() -> Vec<u8> {
    let mut settings = vec![0; 256];
    settings[0] = 8;
    settings[2] = 10;
    settings[3] = 4;
    settings[4] = 20;
    settings[5] = 0;
    settings[6] = 2;
    settings[10] = 1;
    settings[13] = 1;
    settings[16] = 1;
    settings[17] = 1;
    settings[19] = 1;
    settings
}

pub fn normalize_settings(settings: Option<Vec<u8>>) -> Vec<u8> {
    match settings {
        Some(bytes) if bytes.len() == 256 && bytes.first() == Some(&8) => bytes,
        Some(bytes) => {
            let mut settings = legacy_default_settings();
            for (idx, byte) in bytes.into_iter().take(256).enumerate() {
                settings[idx] = byte;
            }
            settings[0] = 8;
            settings
        }
        None => legacy_default_settings(),
    }
}

pub fn handle_read_only_command(
    command: ManagerCommand,
    state: &ReadOnlyState,
) -> Result<Vec<u8>, DaemonError> {
    match command {
        ManagerCommand::GetPropInfo => Ok(format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            state.module_id,
            state.module_name,
            state.version,
            state.version_code,
            state.author,
            state.cluster_num,
            state.module_env,
            state.work_mode,
            state.android_version,
            state.kernel_version,
            state.ext_memory_mib,
            state.daemon_health,
            state.hook_health
        )
        .into_bytes()),
        ManagerCommand::GetChangelog => Ok(state.changelog.as_bytes().to_vec()),
        ManagerCommand::GetLog => {
            let raw = state
                .settings
                .get(LOG_LEVEL_SETTING_INDEX)
                .copied()
                .unwrap_or(0);
            let (view, _) = decode_log_view(raw);
            Ok(state.manager_log.render(view).into_bytes())
        }
        ManagerCommand::GetSettings => Ok(state.settings.clone()),
        ManagerCommand::GetXpLog => Ok(state.xp_log.as_bytes().to_vec()),
        ManagerCommand::GetAppCfg => Ok(encode_app_config_for_manager(&state.app_config)),
        ManagerCommand::GetUidTime => Ok(Vec::new()),
        ManagerCommand::SetAppCfg
        | ManagerCommand::SetAppLabel
        | ManagerCommand::SetSettingsVar => Err(DaemonError::protocol(
            "write command requires mutable manager state",
        )),
        ManagerCommand::GetHealthReport => Ok(state.health_report_json.as_bytes().to_vec()),
        ManagerCommand::GetCapabilityReport => Ok(state.capability_report_json.as_bytes().to_vec()),
        ManagerCommand::GetCompatibilityBaseline => {
            Ok(state.compatibility_report_json.as_bytes().to_vec())
        }
        ManagerCommand::GetOperationLogJson => Ok(state.operation_log_json.as_bytes().to_vec()),
        ManagerCommand::RunSelfCheck => Ok(state.self_check_json.as_bytes().to_vec()),
        _ => Err(DaemonError::protocol(format!(
            "manager command {command:?} is not implemented in the compatibility handler"
        ))),
    }
}

pub fn handle_manager_command(
    frame: &ManagerFrame,
    state: &mut ReadOnlyState,
    mut set_app_config: impl FnMut(&[u8]) -> Result<bool, DaemonError>,
) -> Result<Vec<u8>, DaemonError> {
    match frame.command {
        ManagerCommand::SetAppCfg => set_app_config_with_persistence_writer(
            state,
            &frame.payload,
            &mut set_app_config,
            |path, payload| atomic_write(path, payload),
        ),
        ManagerCommand::SetAppLabel => {
            let labels = decode_app_label_payload(&frame.payload);
            let path = state.app_label_path.as_ref().ok_or_else(|| {
                DaemonError::config("app label persistence path is not configured")
            })?;
            atomic_write(path, &frame.payload)?;
            append_legacy_log_message(state, &format_label_update_log(&labels));
            Ok(b"success".to_vec())
        }
        ManagerCommand::SetSettingsVar => {
            let response = set_settings_var(state, &frame.payload)?;
            if response == b"success" {
                state.hook_config_synced = false;
                append_legacy_log_message(state, "⚙️设置成功");
            } else if let Ok(message) = String::from_utf8(response.clone()) {
                append_legacy_log_message(state, &format!("🔧设置失败，{message}"));
            }
            Ok(response)
        }
        ManagerCommand::GetRealTimeInfo => {
            encode_realtime_info_with_settings(&frame.payload, &state.settings)
        }
        ManagerCommand::GetUidTime => encode_uid_time(state),
        ManagerCommand::GetFreezeStatus => Ok(encode_freeze_status(&state.freeze_status)),
        ManagerCommand::ClearLog => {
            state.manager_log.clear();
            // 返回非空负载：旧版返回 b"\n"。客户端 Logcat.logTask 对 0 字节响应会
            // 提前返回不刷新 UI，导致清屏按钮不清空。这里返回换行使长度检查通过、
            // 触发 TextView 清空。
            Ok(b"\n".to_vec())
        }
        ManagerCommand::GetProcState => {
            append_legacy_proc_state_log(state);
            // 用 LogView::Info 直接渲染，不受 settings[30] 日志级别过滤——proc-state
            // 摘要以 Info/LegacyOperation 追加，在 Warn/Error/Critical 视图下会被
            // record_is_visible 过滤掉导致 check 按钮无响应。
            Ok(state.manager_log.render(LogView::Info).into_bytes())
        }
        _ => handle_read_only_command(frame.command, state),
    }
}

fn set_app_config_with_persistence_writer(
    state: &mut ReadOnlyState,
    payload: &[u8],
    mut set_app_config: impl FnMut(&[u8]) -> Result<bool, DaemonError>,
    persist_config: impl FnOnce(&str, &[u8]) -> Result<(), DaemonError>,
) -> Result<Vec<u8>, DaemonError> {
    let previous_records = state.app_config.clone();
    let previous_xposed_payload =
        encode_xposed_config_payload(&state.settings, &encode_app_config(&previous_records))?;
    let requested_records = decode_app_config(payload)?;
    let records = merge_app_config_records(&previous_records, &requested_records);
    let xposed_payload =
        encode_xposed_config_payload(&state.settings, &encode_app_config(&records))?;
    let previous_file = read_existing_file(state.app_config_path.as_deref())?;
    let source_tokens = match persist_app_config_with_writer(
        state,
        &previous_records,
        &records,
        previous_file.as_deref(),
        persist_config,
    ) {
        Ok(AppConfigPersistenceOutcome::Durable(source_tokens)) => source_tokens,
        Ok(AppConfigPersistenceOutcome::CommittedButUnconfirmed(source_tokens)) => {
            commit_app_config(state, records, source_tokens);
            state.hook_config_synced = false;
            append_legacy_log_message(
                state,
                "配置写入未确认完成，但磁盘已更新；Xposed配置将在后台重新同步",
            );
            return Ok(b"failure".to_vec());
        }
        Err(error) => {
            state.hook_config_synced = false;
            return Err(error);
        }
    };

    match set_app_config(&xposed_payload) {
        Ok(true) => {
            if let Some(message) = format_config_change_log(&previous_records, &records) {
                append_legacy_log_message(state, &message);
            }
            commit_app_config(state, records, source_tokens);
            state.hook_config_synced = true;
            Ok(b"success".to_vec())
        }
        Ok(false) => {
            if let Err(error) = rollback_failed_app_config_update(
                state,
                previous_file.as_deref(),
                &previous_xposed_payload,
                &mut set_app_config,
            ) {
                append_legacy_log_message(state, &format!("配置更新失败，回滚未完成：{error}"));
                return Err(error);
            }
            append_legacy_log_message(state, "配置更新失败：Xposed拒绝新的应用配置");
            Ok(b"failure".to_vec())
        }
        Err(error) => match rollback_failed_app_config_update(
            state,
            previous_file.as_deref(),
            &previous_xposed_payload,
            &mut set_app_config,
        ) {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(DaemonError::system(format!(
                "xposed config update failed: {error}; rollback failed: {rollback_error}"
            ))),
        },
    }
}

fn encode_freeze_status(records: &[ManagerFreezeStatusRecord]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(records.len() * 20);
    for record in records {
        payload.extend_from_slice(&(record.uid as i32).to_le_bytes());
        payload.extend_from_slice(&i32::from(record.foreground).to_le_bytes());
        payload.extend_from_slice(&record.state.to_le_bytes());
        payload.extend_from_slice(&record.seconds.max(0).to_le_bytes());
        payload.extend_from_slice(&record.process_count.max(0).to_le_bytes());
    }
    payload
}

fn append_legacy_proc_state_log(state: &mut ReadOnlyState) {
    let summary = if state.freeze_status.is_empty() {
        "进程冻结状态:\n\n PID | MiB |  状 态  | 进 程\n后台很干净，一个黑名单应用都没有".to_owned()
    } else {
        let rows = state
            .freeze_status
            .iter()
            .map(|record| {
                format!(
                    "{} | {} | {} | {} | {}",
                    record.uid,
                    if record.foreground {
                        "前台"
                    } else {
                        "后台"
                    },
                    legacy_freeze_status_name(record.state),
                    record.seconds.max(0),
                    record.process_count.max(0),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("进程冻结状态:\n\n UID | 前台 | 状态 | 秒 | 进程\n{rows}")
    };
    append_legacy_log_message(state, &summary);
}

fn legacy_freeze_status_name(state: i32) -> &'static str {
    match state {
        0 => "后台运行",
        1 => "前台",
        2 => "等待冻结",
        3 => "已冻结",
        _ => "未知",
    }
}

fn append_legacy_log_message(state: &mut ReadOnlyState, message: &str) {
    let timestamp_ms = current_timestamp_ms();
    state
        .manager_log
        .push(LogRecord::legacy_text(timestamp_ms, message));
}

fn current_timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn format_config_change_log(
    previous_records: &[ManagerAppConfigRecord],
    records: &[ManagerAppConfigRecord],
) -> Option<String> {
    let previous_by_uid = previous_records
        .iter()
        .map(|record| (record.uid, record))
        .collect::<BTreeMap<_, _>>();
    let changes = records
        .iter()
        .filter(|record| record.uid != u32::MAX)
        .filter_map(|record| {
            let previous = previous_by_uid.get(&record.uid)?;
            (previous.mode != record.mode || previous.permissive != record.permissive).then(|| {
                let permissive_change = if previous.permissive != record.permissive {
                    format!(
                        " 宽松:{}->{}",
                        previous.permissive as u8, record.permissive as u8
                    )
                } else {
                    String::new()
                };
                format!(
                    "{}->{} [{}uid{}]{}",
                    previous.mode, record.mode, record.uid, record.uid, permissive_change
                )
            })
        })
        .collect::<Vec<_>>();

    (!changes.is_empty()).then(|| format!("配置变化：\n\n{}", changes.join("\n")))
}

fn decode_app_label_payload(payload: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(payload)
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let mut parts = line.splitn(2, char::is_whitespace);
            let _uid = parts.next()?;
            let label = parts.next()?.trim();
            (!label.is_empty()).then(|| label.to_owned())
        })
        .collect()
}

fn format_label_update_log(labels: &[String]) -> String {
    if labels.is_empty() {
        return "更新 0 款应用名称".to_owned();
    }
    let label_text = labels
        .iter()
        .map(|label| format!("[{}]", sanitize_legacy_log_field(label)))
        .collect::<Vec<_>>()
        .join(" ");
    format!("更新 {} 款应用名称:\n\n{label_text}", labels.len())
}

fn sanitize_legacy_log_field(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '\n' | '\r' | '\t' => ' ',
            other => other,
        })
        .collect()
}

pub fn encode_xposed_config_payload(
    settings: &[u8],
    app_config_payload: &[u8],
) -> Result<Vec<u8>, DaemonError> {
    let records = decode_app_config(app_config_payload)?;
    let settings_line = settings
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let managed_line = records
        .iter()
        .filter(|record| record.mode != 40 && record.mode != 50)
        .map(|record| {
            if record.uid > LEGACY_XPOSED_MAX_UID {
                return Err(DaemonError::protocol(format!(
                    "uid {} exceeds the legacy hook five-digit UID format",
                    record.uid
                )));
            }
            Ok(format!("{:05}uid{}", record.uid, record.uid))
        })
        .collect::<Result<Vec<_>, _>>()?
        .join(" ");
    let permissive_line = records
        .iter()
        .filter(|record| record.permissive && record.mode != 40 && record.mode != 50)
        .map(|record| record.uid.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let managed_line = if managed_line.is_empty() {
        " ".to_owned()
    } else {
        managed_line
    };
    let permissive_line = if permissive_line.is_empty() {
        " ".to_owned()
    } else {
        permissive_line
    };

    Ok(format!("{settings_line}\n{managed_line}\n{permissive_line}").into_bytes())
}

const LEGACY_XPOSED_MAX_UID: u32 = 99_999;

pub fn encode_app_config(records: &[ManagerAppConfigRecord]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(records.len() * 12);
    for record in records {
        bytes.extend_from_slice(&record.uid.to_le_bytes());
        bytes.extend_from_slice(&record.mode.to_le_bytes());
        bytes.extend_from_slice(&(record.permissive as i32).to_le_bytes());
    }
    bytes
}

fn encode_app_config_for_manager(records: &[ManagerAppConfigRecord]) -> Vec<u8> {
    if records.is_empty() {
        return encode_app_config(&[ManagerAppConfigRecord {
            uid: u32::MAX,
            mode: 50,
            permissive: false,
        }]);
    }

    encode_app_config(records)
}

pub fn decode_app_config(payload: &[u8]) -> Result<Vec<ManagerAppConfigRecord>, DaemonError> {
    if !payload.len().is_multiple_of(12) {
        return Err(DaemonError::protocol(
            "app config payload length is not a multiple of 12",
        ));
    }

    Ok(payload
        .chunks_exact(12)
        .map(|chunk| ManagerAppConfigRecord {
            uid: u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]),
            mode: i32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]),
            permissive: i32::from_le_bytes([chunk[8], chunk[9], chunk[10], chunk[11]]) != 0,
        })
        .collect())
}

fn set_settings_var(state: &mut ReadOnlyState, payload: &[u8]) -> Result<Vec<u8>, DaemonError> {
    set_settings_var_with_writer(state, payload, |path, settings| {
        atomic_write(path, settings)
    })
}

fn set_settings_var_with_writer(
    state: &mut ReadOnlyState,
    payload: &[u8],
    persist_settings: impl FnOnce(&str, &[u8]) -> Result<(), DaemonError>,
) -> Result<Vec<u8>, DaemonError> {
    if payload.len() != 2 {
        return Ok(format!("数据长度不正确, 正常:2, 收到:{}", payload.len()).into_bytes());
    }

    let idx = payload[0] as usize;
    let val = payload[1];
    let error = match idx {
        2 if !(1..=60).contains(&val) => Some(format!("超时冻结参数错误, 欲设为:{val}")),
        3 if val > 5 => Some(format!("定时解冻参数错误 欲设为:{val}")),
        4 if !(3..=120).contains(&val) => Some(format!("超时杀死参数错误, 欲设为:{val}")),
        5 if val > 2 => Some(format!("冻结模式参数错误, 欲设为:{val}")),
        6 if val > 3 => Some(format!("定时压制参数错误, 欲设为:{val}")),
        30 if val > 4 => Some(format!("日志级别无效, 正常范围:0..4, 欲设为:{val}")),
        10..=29 if val != 0 && val != 1 => Some(format!("开关值错误, 正常范围:0/1, 欲设为:{val}")),
        2 | 3 | 4 | 5 | 6 | 10..=30 => None,
        _ => Some(format!("设置项不存在, [{idx}]:[{val}]")),
    };
    if let Some(error) = error {
        return Ok(error.into_bytes());
    }

    let mut updated_settings = if state.settings.len() == 256 {
        state.settings.clone()
    } else {
        normalize_settings(Some(state.settings.clone()))
    };
    updated_settings[idx] = val;
    let Some(path) = state.settings_path.clone() else {
        state.hook_config_synced = false;
        return Ok(
            format!("写入设置文件失败, [{idx}]:{val}: persistence path missing").into_bytes(),
        );
    };
    if let Err(error) = persist_settings(&path, &updated_settings) {
        // The write may have failed after the rename or while syncing its parent;
        // read back the target before deciding whether memory still represents the
        // persistent config. The manager still receives a failure because the
        // durability acknowledgement failed, but a later hook sync must use the
        // bytes that actually reached the target path.
        if persisted_bytes_match(&path, &updated_settings) {
            commit_settings(state, updated_settings);
        }
        state.hook_config_synced = false;
        return Ok(format!("写入设置文件失败, [{idx}]:{val}: {error}").into_bytes());
    }

    commit_settings(state, updated_settings);

    Ok(b"success".to_vec())
}

fn persisted_bytes_match(path: &str, expected: &[u8]) -> bool {
    fs::read(path)
        .map(|persisted| persisted == expected)
        .unwrap_or(false)
}

fn commit_settings(state: &mut ReadOnlyState, settings: Vec<u8>) {
    let changed = state.settings != settings;
    state.settings = settings;
    if changed {
        advance_config_revision(state);
    }
}

fn commit_app_config(
    state: &mut ReadOnlyState,
    records: Vec<ManagerAppConfigRecord>,
    source_tokens: BTreeMap<u32, String>,
) {
    let changed = state.app_config != records;
    state.app_config = records;
    state.app_config_source_tokens = source_tokens;
    if changed {
        advance_config_revision(state);
    }
}

fn advance_config_revision(state: &mut ReadOnlyState) {
    state.config_revision = state.config_revision.wrapping_add(1);
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AppConfigPersistenceOutcome {
    Durable(BTreeMap<u32, String>),
    CommittedButUnconfirmed(BTreeMap<u32, String>),
}

fn persist_app_config_with_writer(
    state: &ReadOnlyState,
    previous_records: &[ManagerAppConfigRecord],
    records: &[ManagerAppConfigRecord],
    previous_file: Option<&[u8]>,
    persist_config: impl FnOnce(&str, &[u8]) -> Result<(), DaemonError>,
) -> Result<AppConfigPersistenceOutcome, DaemonError> {
    let Some(path) = &state.app_config_path else {
        return Err(DaemonError::config(
            "app config persistence path is not configured",
        ));
    };
    let records_by_uid = records
        .iter()
        .filter(|record| record.uid != u32::MAX)
        .map(|record| (record.uid, record))
        .collect::<BTreeMap<_, _>>();
    let previous_uids = previous_records
        .iter()
        .filter(|record| record.uid != u32::MAX)
        .map(|record| record.uid)
        .collect::<BTreeSet<_>>();
    let previous_by_uid = previous_records
        .iter()
        .filter(|record| record.uid != u32::MAX)
        .map(|record| (record.uid, record))
        .collect::<BTreeMap<_, _>>();
    let mut source_tokens = state.app_config_source_tokens.clone();
    source_tokens.retain(|uid, _| previous_uids.contains(uid));

    let mut payload = String::new();
    let mut persisted_uids = BTreeSet::new();
    if let Some(previous_file) = previous_file {
        if records == previous_records {
            return Ok(AppConfigPersistenceOutcome::Durable(source_tokens));
        }
        let previous_text = std::str::from_utf8(previous_file)
            .map_err(|_| DaemonError::config("existing app config is not valid UTF-8"))?;
        let lines = previous_text
            .split_inclusive('\n')
            .enumerate()
            .map(|(index, raw_line)| {
                let (line, line_ending) = split_line_ending(raw_line);
                (
                    index,
                    raw_line,
                    line,
                    line_ending,
                    parse_legacy_policy_line(line),
                )
            })
            .collect::<Vec<_>>();

        let source_uid_by_token = source_tokens
            .iter()
            .map(|(uid, token)| (token.clone(), *uid))
            .collect::<BTreeMap<_, _>>();
        let mut line_uids = BTreeMap::new();
        let mut matched_uids = BTreeSet::new();
        for (index, _raw_line, _line, _line_ending, parsed) in &lines {
            let Some(parsed) = parsed else {
                continue;
            };
            let uid = source_uid_by_token
                .get(parsed.package_or_uid.as_str())
                .copied()
                .or_else(|| {
                    parse_legacy_uid_token(&parsed.package_or_uid)
                        .filter(|uid| previous_by_uid.contains_key(uid))
                });
            if let Some(uid) = uid {
                line_uids.insert(*index, uid);
                matched_uids.insert(uid);
                source_tokens.insert(uid, parsed.package_or_uid.clone());
            }
        }

        // States created by older daemons did not retain source tokens. Preserve
        // their legacy positional behavior only for records not anchored by a
        // canonical UID token; once startup has a source map, unmatched rows are
        // intentionally left untouched as unavailable/stale package entries.
        if state.app_config_source_tokens.is_empty() {
            let remaining_uids = previous_records
                .iter()
                .filter(|record| record.uid != u32::MAX && !matched_uids.contains(&record.uid))
                .map(|record| record.uid)
                .collect::<Vec<_>>();
            let mut remaining_uids = remaining_uids.into_iter();
            for (index, _raw_line, _line, _line_ending, parsed) in &lines {
                if parsed.is_some() && !line_uids.contains_key(index) {
                    let Some(uid) = remaining_uids.next() else {
                        break;
                    };
                    line_uids.insert(*index, uid);
                    matched_uids.insert(uid);
                    if let Some(parsed) = parsed {
                        source_tokens.insert(uid, parsed.package_or_uid.clone());
                    }
                }
            }
        }

        for (index, raw_line, _line, line_ending, parsed) in lines {
            let Some(parsed) = parsed else {
                payload.push_str(raw_line);
                continue;
            };
            let Some(uid) = line_uids.get(&index).copied() else {
                payload.push_str(raw_line);
                continue;
            };
            let Some(previous_record) = previous_by_uid.get(&uid).copied() else {
                payload.push_str(raw_line);
                continue;
            };
            persisted_uids.insert(uid);
            let record = records_by_uid.get(&uid).copied().unwrap_or(previous_record);
            if record.mode == previous_record.mode
                && record.permissive == previous_record.permissive
            {
                payload.push_str(raw_line);
            } else {
                payload.push_str(&format!(
                    "{} {} {}{}",
                    parsed.package_or_uid,
                    record.mode,
                    i32::from(record.permissive),
                    line_ending
                ));
            }
        }
    }

    let records_to_append = records
        .iter()
        .filter(|record| record.uid != u32::MAX && !persisted_uids.contains(&record.uid));
    for record in records_to_append {
        if !payload.is_empty() && !payload.ends_with('\n') {
            payload.push('\n');
        }
        let token = format!("{:05}uid{}", record.uid, record.uid);
        payload.push_str(&format!(
            "{} {} {}\n",
            token,
            record.mode,
            i32::from(record.permissive)
        ));
        source_tokens.insert(record.uid, token);
    }

    let payload = payload.into_bytes();
    let persistence = match persist_config(path, &payload) {
        Ok(()) => AppConfigPersistenceOutcome::Durable(source_tokens.clone()),
        Err(_) if persisted_bytes_match(path, &payload) => {
            AppConfigPersistenceOutcome::CommittedButUnconfirmed(source_tokens.clone())
        }
        Err(error) => return Err(error),
    };
    source_tokens.retain(|uid, _| records_by_uid.contains_key(uid));
    match persistence {
        AppConfigPersistenceOutcome::Durable(_) => {
            Ok(AppConfigPersistenceOutcome::Durable(source_tokens))
        }
        AppConfigPersistenceOutcome::CommittedButUnconfirmed(_) => Ok(
            AppConfigPersistenceOutcome::CommittedButUnconfirmed(source_tokens),
        ),
    }
}

pub fn source_tokens_from_legacy_config(
    app_config: Option<&str>,
    mut resolve_package_uid: impl FnMut(&str) -> Option<u32>,
) -> BTreeMap<u32, String> {
    app_config
        .into_iter()
        .flat_map(str::lines)
        .filter_map(parse_legacy_policy_line)
        .filter_map(|record| {
            let uid = resolve_package_uid(&record.package_or_uid)
                .or_else(|| parse_legacy_uid_token(&record.package_or_uid))?;
            Some((uid, record.package_or_uid))
        })
        .collect()
}

fn parse_legacy_uid_token(token: &str) -> Option<u32> {
    match parse_legacy_policy_target(token) {
        Some(LegacyPolicyTarget::Uid(uid)) => Some(uid),
        Some(LegacyPolicyTarget::PackageName(_)) | None => None,
    }
}

fn rollback_failed_app_config_update(
    state: &mut ReadOnlyState,
    previous_file: Option<&[u8]>,
    previous_xposed_payload: &[u8],
    set_app_config: &mut impl FnMut(&[u8]) -> Result<bool, DaemonError>,
) -> Result<(), DaemonError> {
    // This must happen before either rollback action: a failed restore leaves
    // disk/hook state uncertain and must never retain a successful sync claim.
    state.hook_config_synced = false;
    let disk_result = restore_file(state.app_config_path.as_deref(), previous_file);
    let hook_result = set_app_config(previous_xposed_payload);

    if let Err(disk_error) = disk_result {
        let detail = match hook_result {
            Ok(true) => format!("disk restore failed: {disk_error}"),
            Ok(false) => format!(
                "disk restore failed: {disk_error}; hook restore rejected the previous config"
            ),
            Err(hook_error) => {
                format!("disk restore failed: {disk_error}; hook restore failed: {hook_error}")
            }
        };
        return Err(DaemonError::system(detail));
    }

    if !matches!(hook_result, Ok(true)) {
        // The persisted state is back to its previous bytes. Preserve the legacy
        // manager `failure` response and let the normal asynchronous sync retry
        // the hook, rather than converting a hook rejection into a transport error.
        append_legacy_log_message(state, "配置回滚：Xposed未确认旧配置，将在后台重试");
    }

    Ok(())
}

fn split_line_ending(line: &str) -> (&str, &str) {
    if let Some(body) = line.strip_suffix("\r\n") {
        (body, "\r\n")
    } else if let Some(body) = line.strip_suffix('\n') {
        (body, "\n")
    } else {
        (line, "")
    }
}

fn merge_app_config_records(
    previous_records: &[ManagerAppConfigRecord],
    requested_records: &[ManagerAppConfigRecord],
) -> Vec<ManagerAppConfigRecord> {
    let requested_by_uid = requested_records
        .iter()
        .filter(|record| record.uid != u32::MAX)
        .map(|record| (record.uid, *record))
        .collect::<BTreeMap<_, _>>();
    let previous_uids = previous_records
        .iter()
        .map(|record| record.uid)
        .collect::<BTreeSet<_>>();
    let mut merged = previous_records
        .iter()
        .map(|record| {
            requested_by_uid
                .get(&record.uid)
                .copied()
                .unwrap_or(*record)
        })
        .collect::<Vec<_>>();
    merged.extend(
        requested_records
            .iter()
            .filter(|record| record.uid != u32::MAX && !previous_uids.contains(&record.uid))
            .copied(),
    );
    merged
}

fn read_existing_file(path: Option<&str>) -> Result<Option<Vec<u8>>, DaemonError> {
    let path = path.ok_or_else(|| DaemonError::config("persistence path is not configured"))?;
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn restore_file(path: Option<&str>, previous: Option<&[u8]>) -> Result<(), DaemonError> {
    let path = path.ok_or_else(|| DaemonError::config("persistence path is not configured"))?;
    match previous {
        Some(bytes) => atomic_write(path, bytes),
        None => match fs::remove_file(path) {
            Ok(()) => sync_parent(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        },
    }
}

fn atomic_write(path: impl AsRef<Path>, payload: &[u8]) -> Result<(), DaemonError> {
    let path = path.as_ref();
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| DaemonError::config("persistence path has no valid file name"))?;
    let temp_path = path.with_file_name(format!(".{file_name}.tmp"));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temp_path)?;
    file.write_all(payload)?;
    file.sync_all()?;
    fs::rename(&temp_path, path)?;
    sync_parent(path)
}

fn sync_parent(path: impl AsRef<Path>) -> Result<(), DaemonError> {
    let parent = path
        .as_ref()
        .parent()
        .ok_or_else(|| DaemonError::config("persistence path has no parent directory"))?;
    OpenOptions::new().read(true).open(parent)?.sync_all()?;
    Ok(())
}

pub fn encode_realtime_info(payload: &[u8]) -> Result<Vec<u8>, DaemonError> {
    encode_realtime_info_with_settings(payload, &legacy_default_settings())
}

pub fn encode_realtime_info_with_settings(
    payload: &[u8],
    settings: &[u8],
) -> Result<Vec<u8>, DaemonError> {
    if let Some(response) = realtime_request_validation_response(payload)? {
        return Ok(response);
    }

    let sample = collect_realtime_sample(settings);
    let sampler = REALTIME_SAMPLER.get_or_init(|| Mutex::new(RealtimeSampler::default()));
    let mut sampler = sampler
        .lock()
        .map_err(|_| DaemonError::protocol("realtime sampler lock is poisoned"))?;
    encode_realtime_info_with_sample(payload, settings, &mut sampler, sample)
}

fn encode_realtime_info_with_sample(
    payload: &[u8],
    _settings: &[u8],
    sampler: &mut RealtimeSampler,
    sample: RealtimeSample,
) -> Result<Vec<u8>, DaemonError> {
    if let Some(response) = realtime_request_validation_response(payload)? {
        return Ok(response);
    }

    // `realtime_request_validation_response` has already checked the payload
    // length, dimensions, and response size before any system sampling.
    let height = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
    let width = u32::from_le_bytes(payload[4..8].try_into().unwrap()) as usize;
    let available_mib = u32::from_le_bytes(payload[8..12].try_into().unwrap()) as i32;
    let image_len = height
        .checked_mul(width)
        .and_then(|pixels| pixels.checked_mul(4))
        .expect("validated realtime image size fits usize");

    let metrics = realtime_metrics_from_sample(available_mib, _settings, sampler, sample);
    let mut response = vec![0; image_len];
    draw_realtime_chart(&mut response, height, width, &metrics, &sampler.history);
    for value in metrics {
        response.extend_from_slice(&value.to_le_bytes());
    }
    Ok(response)
}

fn realtime_request_validation_response(payload: &[u8]) -> Result<Option<Vec<u8>>, DaemonError> {
    if payload.len() != 12 {
        return Ok(Some(
            format!("实时信息需要12字节, 实际收到[{}]", payload.len()).into_bytes(),
        ));
    }

    let height = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
    let width = u32::from_le_bytes(payload[4..8].try_into().unwrap()) as usize;
    if height < 20 || width < 20 {
        return Ok(Some(
            format!("宽高不符合, height[{height}] width[{width}]").into_bytes(),
        ));
    }

    let image_len = height
        .checked_mul(width)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| DaemonError::protocol("realtime image size overflows"))?;
    if !matches!(
        image_len.checked_add(23 * 4),
        Some(response_len) if response_len <= MAX_PAYLOAD_LEN
    ) {
        return Ok(Some(
            format!("实时信息响应过大, height[{height}] width[{width}]").into_bytes(),
        ));
    }

    Ok(None)
}

fn encode_uid_time(state: &mut ReadOnlyState) -> Result<Vec<u8>, DaemonError> {
    let text = fs::read_to_string(&state.uid_time_path).unwrap_or_default();
    let managed_uids = state
        .app_config
        .iter()
        .filter(|record| record.uid != u32::MAX && record.mode != 40 && record.mode != 50)
        .map(|record| record.uid)
        .collect::<BTreeSet<_>>();
    if managed_uids.is_empty() {
        state.uid_time_totals.clear();
        return Ok(Vec::new());
    }

    let mut records = text
        .lines()
        .filter_map(parse_uid_cpu_time_line)
        .filter(|(uid, total_ms)| managed_uids.contains(uid) && *total_ms > 0)
        .map(|(uid, total_ms)| {
            let last_total = state.uid_time_totals.insert(uid, total_ms).unwrap_or(0);
            let delta_ms = total_ms.saturating_sub(last_total);
            (uid, delta_ms, total_ms)
        })
        .collect::<Vec<_>>();
    state
        .uid_time_totals
        .retain(|uid, _| managed_uids.contains(uid));
    records.sort_by(|left, right| right.2.cmp(&left.2).then_with(|| left.0.cmp(&right.0)));

    let mut payload = Vec::with_capacity(records.len() * 12);
    for (uid, delta_ms, total_ms) in records {
        payload.extend_from_slice(&(uid as i32).to_le_bytes());
        payload.extend_from_slice(&delta_ms.to_le_bytes());
        payload.extend_from_slice(&total_ms.to_le_bytes());
    }
    Ok(payload)
}

fn parse_uid_cpu_time_line(line: &str) -> Option<(u32, i32)> {
    let (uid_text, times_text) = line.split_once(':')?;
    let uid = uid_text.trim().parse::<u32>().ok()?;
    let mut times = times_text.split_whitespace();
    let user_us = times.next()?.parse::<i64>().ok()?;
    let system_us = times.next()?.parse::<i64>().ok()?;
    let total_ms = ((user_us.saturating_add(system_us)) / 1000).clamp(0, i32::MAX as i64) as i32;
    Some((uid, total_ms))
}

fn draw_realtime_chart(
    image: &mut [u8],
    height: usize,
    width: usize,
    metrics: &[i32; 23],
    history: &CpuHistory,
) {
    const COLOR_BLUE: u32 = 0xBBFF8000;
    const COLOR_GRAY: u32 = 0x01808080;
    const COLOR_CPU: [u32; 8] = [
        0xffddb822, 0xffb8dd22, 0xff6ddd22, 0xff22dd92, 0xff1ae6e6, 0xff1abde6, 0xff1a6be6,
        0xff1a1ae6,
    ];

    let chart_height = (height * 4 / 5).max(1);
    for y in [height / 5, height * 2 / 5, height * 3 / 5] {
        for x in 0..width {
            put_pixel(image, width, x, y.min(height - 1), COLOR_GRAY);
        }
    }
    for i in 1..10 {
        let x = width * i / 10;
        for y in 0..chart_height {
            put_pixel(image, width, x.min(width - 1), y, COLOR_GRAY);
        }
    }

    draw_memory_bars(image, height, width, metrics, COLOR_BLUE, COLOR_GRAY);
    draw_cpu_history(image, chart_height, width, history, &COLOR_CPU);

    for x in 0..width {
        put_pixel(image, width, x, 0, COLOR_BLUE);
        put_pixel(image, width, x, chart_height.min(height - 1), COLOR_BLUE);
    }
    for y in 0..chart_height {
        put_pixel(image, width, 0, y, COLOR_BLUE);
        put_pixel(image, width, width - 1, y, COLOR_BLUE);
    }
}

fn draw_memory_bars(
    image: &mut [u8],
    height: usize,
    width: usize,
    metrics: &[i32; 23],
    used_color: u32,
    free_color: u32,
) {
    let mem_total = metrics[0].max(0) as usize;
    let mem_available = metrics[1].max(0) as usize;
    let swap_total = metrics[2].max(0) as usize;
    let swap_free = metrics[3].max(0) as usize;
    let bar_top = height * 218 / 256;
    let physical_start = width * 5 / 100;
    let physical_end = width * 45 / 100;
    let physical_used_width = (physical_end - physical_start)
        .saturating_mul(mem_total.saturating_sub(mem_available))
        .checked_div(mem_total)
        .unwrap_or(0);
    let physical_used_end = physical_start + physical_used_width;
    fill_horizontal_bar(
        image,
        width,
        bar_top,
        height,
        physical_start,
        physical_end,
        physical_used_end,
        used_color,
        free_color,
    );

    let swap_start = width * 55 / 100;
    let swap_end = width * 95 / 100;
    if let Some(swap_used_width) = (swap_end - swap_start)
        .saturating_mul(swap_total.saturating_sub(swap_free))
        .checked_div(swap_total)
    {
        let swap_used_end = swap_start + swap_used_width;
        fill_horizontal_bar(
            image,
            width,
            bar_top,
            height,
            swap_start,
            swap_end,
            swap_used_end,
            used_color,
            free_color,
        );
    }
}

fn draw_cpu_history(
    image: &mut [u8],
    chart_height: usize,
    width: usize,
    history: &CpuHistory,
    colors: &[u32; CPU_CHART_CORES],
) {
    for (core, color) in colors.iter().enumerate().take(CPU_CHART_CORES) {
        for minute_idx in 1..CPU_HISTORY_BUCKETS {
            let usage0 = history.cores[(history.bucket_idx + minute_idx) % CPU_HISTORY_BUCKETS]
                [core]
                .clamp(0, 100);
            let usage1 = history.cores[(history.bucket_idx + minute_idx + 1) % CPU_HISTORY_BUCKETS]
                [core]
                .clamp(0, 100);
            let y0 = usage_to_chart_y(usage0, chart_height);
            let y1 = usage_to_chart_y(usage1, chart_height);
            let x0 =
                (width * (minute_idx - 1) / (CPU_HISTORY_BUCKETS - 1)).min(width.saturating_sub(1));
            let x1 = (width * minute_idx / (CPU_HISTORY_BUCKETS - 1)).min(width.saturating_sub(1));
            draw_line(image, width, x0, y0, x1, y1, *color);
        }
    }
}

fn usage_to_chart_y(usage: i32, chart_height: usize) -> usize {
    ((100 - usage.clamp(0, 100)) as usize * chart_height / 100).clamp(1, chart_height - 1)
}

#[allow(clippy::too_many_arguments)]
fn fill_horizontal_bar(
    image: &mut [u8],
    width: usize,
    y_start: usize,
    y_end: usize,
    x_start: usize,
    x_end: usize,
    used_end: usize,
    used_color: u32,
    free_color: u32,
) {
    for y in y_start.min(y_end)..y_end {
        for x in x_start.min(width)..x_end.min(width) {
            put_pixel(
                image,
                width,
                x,
                y,
                if x < used_end { used_color } else { free_color },
            );
        }
    }
}

fn draw_line(
    image: &mut [u8],
    width: usize,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    color: u32,
) {
    let dx = x1.saturating_sub(x0).max(1);
    for x in x0..=x1 {
        let t = x.saturating_sub(x0);
        let y = if y1 >= y0 {
            y0 + (y1 - y0) * t / dx
        } else {
            y0 - (y0 - y1) * t / dx
        };
        put_pixel(image, width, x, y, color);
    }
}

fn put_pixel(image: &mut [u8], width: usize, x: usize, y: usize, color: u32) {
    if x >= width {
        return;
    }
    let offset = (y * width + x) * 4;
    if offset + 4 <= image.len() {
        image[offset..offset + 4].copy_from_slice(&color.to_le_bytes());
    }
}

fn realtime_metrics_from_sample(
    available_mib: i32,
    _settings: &[u8],
    sampler: &mut RealtimeSampler,
    sample: RealtimeSample,
) -> [i32; 23] {
    let mut metrics = [0_i32; 23];
    let meminfo = sample.meminfo;
    metrics[0] = meminfo.mem_total;
    metrics[1] = if available_mib > 0 {
        available_mib
    } else {
        meminfo.mem_available
    };
    metrics[2] = meminfo.swap_total;
    metrics[3] = meminfo.swap_free;

    metrics[4..(CPU_CHART_CORES + 4)].copy_from_slice(&sample.frequencies_mhz[..CPU_CHART_CORES]);

    let cpu_usage = sampler.record_proc_stat(&sample.proc_stat);
    metrics[20] = cpu_usage.summary;
    for (core, usage) in cpu_usage.cores.iter().take(CPU_CHART_CORES).enumerate() {
        metrics[12 + core] = *usage;
    }

    metrics[21] = sample.temperature_milli_celsius;
    metrics[22] = sample.battery_power_mw;
    metrics
}

fn collect_realtime_sample(settings: &[u8]) -> RealtimeSample {
    #[cfg(test)]
    REALTIME_SAMPLE_CALLS.with(|calls| calls.set(calls.get() + 1));

    let mut sample = RealtimeSample {
        meminfo: read_meminfo_mib(),
        proc_stat: fs::read_to_string("/proc/stat")
            .map(|text| parse_proc_stat(&text))
            .unwrap_or_default(),
        temperature_milli_celsius: read_cpu_temperature_milli_celsius().unwrap_or(0),
        battery_power_mw: read_battery_power_mw(settings).unwrap_or(0),
        ..RealtimeSample::default()
    };
    for core in 0..CPU_CHART_CORES {
        sample.frequencies_mhz[core] = read_cpu_frequency_mhz(core).unwrap_or(0);
    }
    sample
}

#[derive(Debug, Clone, Default)]
struct RealtimeSample {
    meminfo: MemInfoMib,
    frequencies_mhz: [i32; CPU_CHART_CORES],
    proc_stat: CpuStatSnapshot,
    temperature_milli_celsius: i32,
    battery_power_mw: i32,
}

#[derive(Debug, Clone, Default)]
struct MemInfoMib {
    mem_total: i32,
    mem_available: i32,
    swap_total: i32,
    swap_free: i32,
}

fn read_meminfo_mib() -> MemInfoMib {
    let Ok(text) = fs::read_to_string("/proc/meminfo") else {
        return MemInfoMib::default();
    };
    let mut info = MemInfoMib::default();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else {
            continue;
        };
        let Some(value) = parts.next().and_then(|value| value.parse::<i32>().ok()) else {
            continue;
        };
        let value_mib = value / 1024;
        match key.trim_end_matches(':') {
            "MemTotal" => info.mem_total = value_mib,
            "MemAvailable" => info.mem_available = value_mib,
            "SwapTotal" => info.swap_total = value_mib,
            "SwapFree" => info.swap_free = value_mib,
            _ => {}
        }
    }
    info
}

fn read_cpu_frequency_mhz(core: usize) -> Option<i32> {
    let path = format!("/sys/devices/system/cpu/cpu{core}/cpufreq/scaling_cur_freq");
    read_i64(path).map(|khz| (khz / 1000) as i32)
}

#[derive(Debug, Default)]
struct CpuUsagePercent {
    summary: i32,
    cores: Vec<i32>,
}

#[derive(Debug, Clone, Copy)]
struct ProcStatSample {
    total: u64,
    idle: u64,
}

#[derive(Debug, Clone, Default)]
struct CpuStatSnapshot {
    summary: Option<ProcStatSample>,
    cores: Vec<Option<ProcStatSample>>,
}

#[derive(Debug, Clone)]
struct RealtimeSampler {
    history: CpuHistory,
    last_summary: Option<ProcStatSample>,
    last_cores: Vec<Option<ProcStatSample>>,
}

impl Default for RealtimeSampler {
    fn default() -> Self {
        Self {
            history: CpuHistory::default(),
            last_summary: None,
            last_cores: vec![None; CPU_CHART_CORES],
        }
    }
}

impl RealtimeSampler {
    fn record_proc_stat(&mut self, snapshot: &CpuStatSnapshot) -> CpuUsagePercent {
        self.history.advance();
        let mut usage = CpuUsagePercent {
            summary: cpu_delta_usage_percent(&mut self.last_summary, snapshot.summary),
            cores: vec![0; CPU_CHART_CORES],
        };
        self.history.summary[self.history.bucket_idx] = usage.summary;

        if self.last_cores.len() < CPU_CHART_CORES {
            self.last_cores.resize(CPU_CHART_CORES, None);
        }
        for core in 0..CPU_CHART_CORES {
            let sample = snapshot.cores.get(core).copied().flatten();
            let percent = cpu_delta_usage_percent(&mut self.last_cores[core], sample);
            usage.cores[core] = percent;
            self.history.cores[self.history.bucket_idx][core] = percent;
        }

        usage
    }
}

#[derive(Debug, Clone)]
struct CpuHistory {
    bucket_idx: usize,
    summary: [i32; CPU_HISTORY_BUCKETS],
    cores: [[i32; CPU_CHART_CORES]; CPU_HISTORY_BUCKETS],
}

impl Default for CpuHistory {
    fn default() -> Self {
        Self {
            bucket_idx: 0,
            summary: [0; CPU_HISTORY_BUCKETS],
            cores: [[0; CPU_CHART_CORES]; CPU_HISTORY_BUCKETS],
        }
    }
}

impl CpuHistory {
    fn advance(&mut self) {
        self.bucket_idx = (self.bucket_idx + 1) % CPU_HISTORY_BUCKETS;
        self.summary[self.bucket_idx] = 0;
        self.cores[self.bucket_idx] = [0; CPU_CHART_CORES];
    }
}

fn cpu_delta_usage_percent(
    last_sample: &mut Option<ProcStatSample>,
    current_sample: Option<ProcStatSample>,
) -> i32 {
    let Some(current_sample) = current_sample else {
        return 0;
    };

    let usage = if let Some(last_sample) = *last_sample {
        let total_delta = current_sample.total.saturating_sub(last_sample.total);
        let idle_delta = current_sample.idle.saturating_sub(last_sample.idle);
        if total_delta == 0 || idle_delta > total_delta {
            0
        } else {
            ((total_delta - idle_delta) * 100 / total_delta) as i32
        }
    } else {
        0
    };
    *last_sample = Some(current_sample);
    usage
}

fn parse_proc_stat(text: &str) -> CpuStatSnapshot {
    let mut snapshot = CpuStatSnapshot::default();
    for line in text.lines().filter(|line| line.starts_with("cpu")) {
        let mut parts = line.split_whitespace();
        let Some(label) = parts.next() else {
            continue;
        };
        if label != "cpu" && !label[3..].chars().all(|ch| ch.is_ascii_digit()) {
            continue;
        }
        let values = parts
            .take(7)
            .filter_map(|part| part.parse::<u64>().ok())
            .collect::<Vec<_>>();
        if values.len() < 4 {
            continue;
        }
        let total = values.iter().sum::<u64>();
        let idle = values[3];
        let sample = ProcStatSample { total, idle };

        if label == "cpu" {
            snapshot.summary = Some(sample);
        } else if let Some(core_text) = label.strip_prefix("cpu") {
            if let Ok(core) = core_text.parse::<usize>() {
                if snapshot.cores.len() <= core {
                    snapshot.cores.resize(core + 1, None);
                }
                snapshot.cores[core] = Some(sample);
            }
        }
    }
    snapshot
}

fn read_cpu_temperature_milli_celsius() -> Option<i32> {
    let mut zones = Vec::new();
    for root in ["/sys/class/thermal", "/sys/devices/virtual/thermal"] {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("thermal_zone"))
            {
                continue;
            }
            let Some(temp) = read_i64(path.join("temp")) else {
                continue;
            };
            let zone_type = fs::read_to_string(path.join("type")).unwrap_or_default();
            zones.push((zone_type, temp));
        }
    }

    select_temperature_milli_celsius(zones)
}

fn select_temperature_milli_celsius<I, S>(zones: I) -> Option<i32>
where
    I: IntoIterator<Item = (S, i64)>,
    S: AsRef<str>,
{
    let mut preferred: Option<(u8, i32)> = None;
    let mut fallback: Option<i32> = None;

    for (zone_type, raw_temp) in zones {
        let Some(temp) = normalize_temperature_milli_celsius(raw_temp) else {
            continue;
        };
        let priority = temperature_zone_priority(zone_type.as_ref());
        if priority > 0 {
            let replace = preferred
                .map(|(best_priority, best_temp)| {
                    priority > best_priority || (priority == best_priority && temp > best_temp)
                })
                .unwrap_or(true);
            if replace {
                preferred = Some((priority, temp));
            }
        } else {
            fallback = Some(fallback.map(|best| best.max(temp)).unwrap_or(temp));
        }
    }

    preferred.map(|(_, temp)| temp).or(fallback)
}

fn normalize_temperature_milli_celsius(raw_temp: i64) -> Option<i32> {
    if raw_temp <= 0 {
        return None;
    }
    let temp = if raw_temp < 1_000 {
        raw_temp.saturating_mul(1_000)
    } else {
        raw_temp
    };
    (1_000..=150_000).contains(&temp).then_some(temp as i32)
}

fn temperature_zone_priority(zone_type: &str) -> u8 {
    let zone_type = zone_type.trim().to_ascii_lowercase();
    if zone_type.starts_with("cpu-") || zone_type.starts_with("cpuss") {
        3
    } else if zone_type.contains("soc")
        || zone_type.contains("gpu")
        || zone_type.contains("gpuss")
        || zone_type.contains("aoss")
        || zone_type.contains("ap")
    {
        2
    } else {
        0
    }
}

fn read_battery_power_mw(settings: &[u8]) -> Option<i32> {
    let battery = Path::new("/sys/class/power_supply/battery");
    let usb = Path::new("/sys/class/power_supply/usb");
    battery_power_mw_from_readings(
        read_i64(battery.join("power_now")),
        read_i64(battery.join("voltage_now")),
        read_i64(battery.join("current_now")),
        read_i64(usb.join("voltage_now")),
        read_i64(usb.join("current_now")),
        settings.get(14).copied().unwrap_or(0) != 0,
        settings.get(15).copied().unwrap_or(0) != 0,
    )
}

fn battery_power_mw_from_readings(
    battery_power_uw: Option<i64>,
    battery_voltage_uv: Option<i64>,
    battery_current_ua: Option<i64>,
    usb_voltage_uv: Option<i64>,
    usb_current_ua: Option<i64>,
    enable_current_fix: bool,
    enable_double_cell: bool,
) -> Option<i32> {
    if let Some(power_uw) = battery_power_uw.filter(|power| *power != 0) {
        return Some((power_uw.abs() / 1000) as i32);
    }

    let battery_power_mw =
        battery_voltage_uv
            .zip(battery_current_ua)
            .map(|(voltage_uv, current_raw)| {
                let mut current_ma = if enable_current_fix && current_raw.abs() <= 100_000 {
                    current_raw
                } else {
                    current_raw / 1000
                };
                if enable_double_cell {
                    current_ma *= 2;
                }
                ((voltage_uv.abs() / 1000) * current_ma.abs()) / 1000
            });
    if let Some(power_mw) = battery_power_mw.filter(|power| *power >= 100) {
        return Some(power_mw as i32);
    }

    if let (Some(voltage_uv), Some(current_ua)) = (
        usb_voltage_uv,
        usb_current_ua.filter(|current| *current != 0),
    ) {
        return Some(((voltage_uv * current_ua.abs()) / 1_000_000_000) as i32);
    }

    battery_power_mw.map(|power| power as i32).or(Some(0))
}

fn read_i64(path: impl AsRef<Path>) -> Option<i64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

pub fn checksum(payload: &[u8]) -> u8 {
    payload
        .iter()
        .fold(0, |accumulator, byte| accumulator ^ byte)
}

pub fn parse_frame(bytes: &[u8]) -> Result<ManagerFrame, DaemonError> {
    if bytes.len() < HEADER_LEN {
        return Err(DaemonError::protocol("manager frame header is incomplete"));
    }

    let payload_len = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    if payload_len > MAX_PAYLOAD_LEN {
        return Err(DaemonError::protocol("manager frame payload is too large"));
    }

    let expected_len = HEADER_LEN + payload_len;
    if bytes.len() != expected_len {
        return Err(DaemonError::protocol(format!(
            "manager frame length mismatch: expected {expected_len}, got {}",
            bytes.len()
        )));
    }

    let payload = bytes[HEADER_LEN..].to_vec();
    let expected_checksum = if payload.is_empty() {
        0
    } else {
        checksum(&payload)
    };
    if bytes[5] != expected_checksum {
        return Err(DaemonError::protocol("manager frame checksum mismatch"));
    }

    Ok(ManagerFrame {
        command: ManagerCommand::try_from(bytes[4])?,
        payload,
    })
}

pub fn encode_frame(command: ManagerCommand, payload: &[u8]) -> Result<Vec<u8>, DaemonError> {
    if payload.len() > MAX_PAYLOAD_LEN {
        return Err(DaemonError::protocol("manager frame payload is too large"));
    }

    let mut bytes = Vec::with_capacity(HEADER_LEN + payload.len());
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.push(command as u8);
    bytes.push(if payload.is_empty() {
        0
    } else {
        checksum(payload)
    });
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_power_uses_usb_input_when_full_battery_reports_zero_current() {
        let power = battery_power_mw_from_readings(
            None,
            Some(4_446_000),
            Some(0),
            Some(4_902_000),
            Some(434_000),
            false,
            false,
        );

        assert_eq!(power, Some(2_127));
    }

    #[test]
    fn battery_power_ignores_noise_level_battery_current() {
        let power = battery_power_mw_from_readings(
            Some(0),
            Some(4_439_000),
            Some(-52),
            Some(4_850_000),
            Some(497_000),
            false,
            false,
        );

        assert_eq!(power, Some(2_410));
    }

    #[test]
    fn battery_power_honors_current_fix_milliamp_reading_when_discharging() {
        let power = battery_power_mw_from_readings(
            Some(0),
            Some(4_000_000),
            Some(-300),
            Some(0),
            Some(0),
            true,
            false,
        );

        assert_eq!(power, Some(1_200));
    }

    #[test]
    fn realtime_cpu_usage_uses_proc_stat_deltas() {
        let mut sampler = RealtimeSampler::default();
        let settings = legacy_default_settings();
        let mut sample = RealtimeSample {
            meminfo: MemInfoMib {
                mem_total: 4096,
                mem_available: 1024,
                swap_total: 2048,
                swap_free: 1024,
            },
            proc_stat: parse_proc_stat(
                "cpu  100 0 50 850 0 0 0 0 0 0\n\
                 cpu0 50 0 25 425 0 0 0 0 0 0\n\
                 cpu1 50 0 25 425 0 0 0 0 0 0\n",
            ),
            ..RealtimeSample::default()
        };

        let first = realtime_metrics_from_sample(777, &settings, &mut sampler, sample.clone());

        assert_eq!(first[20], 0);
        assert_eq!(first[12], 0);
        assert_eq!(first[13], 0);
        assert_eq!(first[1], 777);

        sample.proc_stat = parse_proc_stat(
            "cpu  160 0 90 950 0 0 0 0 0 0\n\
             cpu0 70 0 35 505 0 0 0 0 0 0\n\
             cpu1 150 0 25 425 0 0 0 0 0 0\n",
        );
        let second = realtime_metrics_from_sample(777, &settings, &mut sampler, sample);

        assert_eq!(second[20], 50);
        assert_eq!(second[12], 27);
        assert_eq!(second[13], 100);
    }

    #[test]
    fn realtime_chart_uses_persistent_sample_history() {
        let mut sampler = RealtimeSampler::default();
        let settings = legacy_default_settings();
        let mut request = Vec::new();
        request.extend_from_slice(&48_u32.to_le_bytes());
        request.extend_from_slice(&64_u32.to_le_bytes());
        request.extend_from_slice(&123_u32.to_le_bytes());

        let mut sample = RealtimeSample {
            meminfo: MemInfoMib {
                mem_total: 4096,
                mem_available: 2048,
                swap_total: 1024,
                swap_free: 512,
            },
            proc_stat: parse_proc_stat(
                "cpu  100 0 0 900 0 0 0 0 0 0\n\
                 cpu0 100 0 0 900 0 0 0 0 0 0\n",
            ),
            ..RealtimeSample::default()
        };
        let first =
            encode_realtime_info_with_sample(&request, &settings, &mut sampler, sample.clone())
                .expect("first realtime response succeeds");

        sample.proc_stat = parse_proc_stat(
            "cpu  125 0 0 975 0 0 0 0 0 0\n\
             cpu0 125 0 0 975 0 0 0 0 0 0\n",
        );
        let second =
            encode_realtime_info_with_sample(&request, &settings, &mut sampler, sample.clone())
                .expect("second realtime response succeeds");

        sample.proc_stat = parse_proc_stat(
            "cpu  200 0 0 1000 0 0 0 0 0 0\n\
             cpu0 200 0 0 1000 0 0 0 0 0 0\n",
        );
        let third = encode_realtime_info_with_sample(&request, &settings, &mut sampler, sample)
            .expect("third realtime response succeeds");

        let image_len = 48 * 64 * 4;
        assert_ne!(&first[..image_len], &second[..image_len]);
        assert_ne!(&second[..image_len], &third[..image_len]);
        assert_eq!(
            i32::from_le_bytes(
                third[image_len + 20 * 4..image_len + 21 * 4]
                    .try_into()
                    .unwrap()
            ),
            75
        );
        assert_eq!(
            i32::from_le_bytes(
                third[image_len + 12 * 4..image_len + 13 * 4]
                    .try_into()
                    .unwrap()
            ),
            75
        );
    }

    #[test]
    fn realtime_temperature_ignores_zero_bcl_and_prefers_cpu_zone() {
        let zones = vec![
            ("pm8550-bcl-lvl0".to_owned(), 0),
            ("aoss-0".to_owned(), 36_900),
            ("cpu-0-0-0".to_owned(), 40_300),
            ("cpuss-1-1".to_owned(), 38_200),
        ];

        assert_eq!(select_temperature_milli_celsius(zones), Some(40_300));
    }

    #[test]
    fn settings_write_failure_keeps_memory_unchanged_and_marks_hook_unsynced() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut state = ReadOnlyState::default();
        let previous_settings = state.settings.clone();
        state.settings_path = Some(temp.path().to_string_lossy().into_owned());
        state.hook_config_synced = true;
        let frame = ManagerFrame {
            command: ManagerCommand::SetSettingsVar,
            payload: vec![13, 0],
        };

        let response = handle_manager_command(&frame, &mut state, |_| Ok(true))
            .expect("persistence failure is returned to the manager");

        assert_ne!(response, b"success");
        assert_eq!(state.settings, previous_settings);
        assert!(!state.hook_config_synced);
    }

    #[test]
    fn settings_post_commit_write_failure_reconciles_memory_and_advances_revision() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("settings.db");
        let mut state = ReadOnlyState::default();
        state.settings_path = Some(path.to_string_lossy().into_owned());
        state.hook_config_synced = true;
        let previous_revision = state.config_revision;

        let response = set_settings_var_with_writer(&mut state, &[13, 0], |path, bytes| {
            fs::write(path, bytes).expect("simulate the completed rename");
            Err(DaemonError::system("parent directory sync failed"))
        })
        .expect("post-commit persistence failure is returned to the manager");

        assert_ne!(response, b"success");
        assert_eq!(state.settings[13], 0);
        assert_eq!(
            fs::read(&path).expect("read committed settings"),
            state.settings
        );
        assert_eq!(state.config_revision, previous_revision + 1);
        assert!(!state.hook_config_synced);
    }

    #[test]
    fn app_config_post_commit_write_failure_reconciles_memory_and_invalidates_hook_sync() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("appcfg.txt");
        fs::write(&path, "10000uid10000 30 0\n").expect("seed app config");
        let mut state = ReadOnlyState::default();
        state.app_config_path = Some(path.to_string_lossy().into_owned());
        state.app_config = vec![ManagerAppConfigRecord {
            uid: 10_000,
            mode: 30,
            permissive: false,
        }];
        state.hook_config_synced = true;
        let previous_revision = state.config_revision;
        let requested = encode_app_config(&[ManagerAppConfigRecord {
            uid: 10_000,
            mode: 20,
            permissive: false,
        }]);
        let hook_calls = std::cell::Cell::new(0);

        let response = set_app_config_with_persistence_writer(
            &mut state,
            &requested,
            |_| {
                hook_calls.set(hook_calls.get() + 1);
                Ok(true)
            },
            |path, bytes| {
                fs::write(path, bytes).expect("simulate the completed rename");
                Err(DaemonError::system("parent directory sync failed"))
            },
        )
        .expect("post-commit persistence failure keeps the daemon coherent");

        assert_eq!(response, b"failure");
        assert_eq!(
            hook_calls.get(),
            0,
            "unconfirmed write must not sync the hook"
        );
        assert_eq!(
            state.app_config,
            vec![ManagerAppConfigRecord {
                uid: 10_000,
                mode: 20,
                permissive: false,
            }]
        );
        assert_eq!(
            fs::read_to_string(&path).expect("read committed app config"),
            "10000uid10000 20 0\n"
        );
        assert_eq!(state.config_revision, previous_revision + 1);
        assert!(!state.hook_config_synced);
    }

    #[test]
    fn config_revision_tracks_only_committed_manager_config_mutations() {
        let temp = tempfile::tempdir().expect("tempdir");
        let app_config_path = temp.path().join("appcfg.txt");
        fs::write(&app_config_path, "10000uid10000 30 0\n").expect("seed app config");
        let settings_path = temp.path().join("settings.db");
        let mut state = ReadOnlyState::default();
        state.app_config_path = Some(app_config_path.to_string_lossy().into_owned());
        state.settings_path = Some(settings_path.to_string_lossy().into_owned());
        state.app_config = vec![ManagerAppConfigRecord {
            uid: 10_000,
            mode: 30,
            permissive: false,
        }];
        let change = ManagerFrame {
            command: ManagerCommand::SetAppCfg,
            payload: encode_app_config(&[ManagerAppConfigRecord {
                uid: 10_000,
                mode: 20,
                permissive: false,
            }]),
        };

        let rejected = handle_manager_command(&change, &mut state, |_| Ok(false))
            .expect("rejected app config has a legacy failure response");
        assert_eq!(rejected, b"failure");
        assert_eq!(state.config_revision, 0);

        let applied = handle_manager_command(&change, &mut state, |_| Ok(true))
            .expect("app config update succeeds");
        assert_eq!(applied, b"success");
        assert_eq!(state.config_revision, 1);

        let repeated = handle_manager_command(&change, &mut state, |_| Ok(true))
            .expect("identical app config update succeeds");
        assert_eq!(repeated, b"success");
        assert_eq!(state.config_revision, 1);

        let setting = ManagerFrame {
            command: ManagerCommand::SetSettingsVar,
            payload: vec![13, 0],
        };
        let setting_applied = handle_manager_command(&setting, &mut state, |_| Ok(true))
            .expect("settings update succeeds");
        assert_eq!(setting_applied, b"success");
        assert_eq!(state.config_revision, 2);

        let setting_repeated = handle_manager_command(&setting, &mut state, |_| Ok(true))
            .expect("identical settings update succeeds");
        assert_eq!(setting_repeated, b"success");
        assert_eq!(state.config_revision, 2);
    }

    #[test]
    fn app_config_save_skips_unloaded_policy_lines_when_aligning_known_uid_rows() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("appcfg.txt");
        fs::write(&path, "com.example.uninstalled 30 0\n10000uid10000 30 0\n")
            .expect("seed app config");
        let mut state = ReadOnlyState::default();
        state.app_config_path = Some(path.to_string_lossy().into_owned());
        state.app_config = vec![ManagerAppConfigRecord {
            uid: 10_000,
            mode: 30,
            permissive: false,
        }];
        let frame = ManagerFrame {
            command: ManagerCommand::SetAppCfg,
            payload: encode_app_config(&[ManagerAppConfigRecord {
                uid: 10_000,
                mode: 20,
                permissive: false,
            }]),
        };

        let response = handle_manager_command(&frame, &mut state, |_| Ok(true))
            .expect("valid UID record remains writable despite a stale package line");

        assert_eq!(response, b"success");
        assert_eq!(
            fs::read_to_string(path).expect("read persisted config"),
            "com.example.uninstalled 30 0\n10000uid10000 20 0\n"
        );
    }

    #[test]
    fn app_config_rollback_replays_old_hook_config_even_when_disk_restore_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("appcfg.txt");
        fs::write(&path, "10000uid10000 30 0\n").expect("seed app config");
        let mut state = ReadOnlyState::default();
        state.app_config_path = Some(path.to_string_lossy().into_owned());
        state.app_config = vec![ManagerAppConfigRecord {
            uid: 10_000,
            mode: 30,
            permissive: false,
        }];
        state.hook_config_synced = true;
        let frame = ManagerFrame {
            command: ManagerCommand::SetAppCfg,
            payload: encode_app_config(&[ManagerAppConfigRecord {
                uid: 10_000,
                mode: 20,
                permissive: false,
            }]),
        };
        let mut calls = 0;

        let result = handle_manager_command(&frame, &mut state, |_| {
            calls += 1;
            if calls == 1 {
                fs::remove_file(&path).expect("remove persisted config before rollback");
                fs::create_dir(&path).expect("replace config path with directory");
                Ok(false)
            } else {
                Ok(true)
            }
        });

        assert!(result.is_err(), "failed disk rollback must be surfaced");
        assert_eq!(calls, 2, "old hook config must still be replayed");
        assert!(!state.hook_config_synced);
    }

    #[test]
    fn proc_state_summary_uses_live_freeze_status_instead_of_a_fixed_clean_message() {
        let mut state = ReadOnlyState::default();
        state.freeze_status = vec![ManagerFreezeStatusRecord {
            uid: 10_042,
            foreground: false,
            state: 3,
            seconds: 12,
            process_count: 2,
        }];
        let frame = ManagerFrame {
            command: ManagerCommand::GetProcState,
            payload: Vec::new(),
        };

        let response = handle_manager_command(&frame, &mut state, |_| Ok(true))
            .expect("proc-state request succeeds");
        let text = String::from_utf8(response).expect("proc-state output is UTF-8");

        assert!(text.contains("10042"));
        assert!(text.contains("冻结"));
        assert!(!text.contains("后台很干净，一个黑名单应用都没有"));
    }

    #[test]
    fn invalid_realtime_request_does_not_collect_system_metrics() {
        REALTIME_SAMPLE_CALLS.with(|calls| calls.set(0));

        let response = encode_realtime_info_with_settings(&[], &legacy_default_settings())
            .expect("malformed realtime request receives a legacy error payload");

        assert!(String::from_utf8_lossy(&response).contains("实时信息需要12字节"));
        REALTIME_SAMPLE_CALLS.with(|calls| {
            assert_eq!(
                calls.get(),
                0,
                "invalid requests must not sample proc/sysfs data"
            )
        });
    }

    #[test]
    fn xposed_config_rejects_uid_that_legacy_hook_cannot_parse() {
        let payload = encode_app_config(&[ManagerAppConfigRecord {
            uid: 110_000,
            mode: 30,
            permissive: false,
        }]);

        let error = encode_xposed_config_payload(&legacy_default_settings(), &payload)
            .expect_err("work-profile UID must not be truncated to five digits");

        assert!(error.to_string().contains("five-digit"));
    }
}
