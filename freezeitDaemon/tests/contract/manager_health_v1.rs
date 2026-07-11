#![allow(clippy::field_reassign_with_default)]

use freezeit_daemon::{
    app::compatibility::RuntimeEnvironment,
    app::controller::{
        read_only_state_with_diagnostics, startup_read_only_state_from_paths,
        sync_loaded_config_to_hook, DiagnosticState,
    },
    app::error::DaemonError,
    app::health::{HealthStatus, ModuleHealth},
    app::logging::{LogLevel, LogRecord, LogView},
    app::operation_log::OperationLog,
    config::loader::DaemonPaths,
    domain::capability::{CapabilityName, ControlCapability},
    protocol::manager_v1::{
        encode_app_config, encode_frame, encode_xposed_config_payload, handle_manager_command,
        handle_read_only_command, parse_frame, ManagerAppConfigRecord, ManagerCommand,
        ReadOnlyState,
    },
    protocol::xposed::{classify_bridge_error, HookBridgeStatus},
};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

fn manager_frame(
    command: ManagerCommand,
    payload: &[u8],
) -> freezeit_daemon::protocol::manager_v1::ManagerFrame {
    parse_frame(&encode_frame(command, payload).unwrap()).expect("manager frame parses")
}

#[test]
fn get_prop_info_returns_legacy_six_line_payload() {
    let state = ReadOnlyState::default();

    let payload =
        handle_read_only_command(ManagerCommand::GetPropInfo, &state).expect("prop info succeeds");
    let text = String::from_utf8(payload).expect("payload is utf-8");
    let lines = text.lines().collect::<Vec<_>>();

    assert!(lines.len() >= 6);
    assert_eq!(lines[0], "freezeit");
    assert_eq!(lines[1], "Freezeit");
    assert_eq!(lines[11], "degraded");
    assert_eq!(lines[12], "unknown");
}

#[test]
fn get_settings_returns_legacy_256_byte_block() {
    let state = ReadOnlyState::default();

    let payload =
        handle_read_only_command(ManagerCommand::GetSettings, &state).expect("settings succeeds");

    assert_eq!(payload.len(), 256);
    assert_eq!(payload[0], 8);
    assert_eq!(payload[2], 10);
    assert_eq!(payload[3], 4);
    assert_eq!(payload[4], 20);
    assert_eq!(payload[13], 1);
}

#[test]
fn app_config_read_and_write_remain_manager_compatible() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut state = ReadOnlyState::default();
    state.app_config_path = Some(
        temp.path()
            .join("appcfg.txt")
            .to_string_lossy()
            .into_owned(),
    );
    state.app_config = vec![
        freezeit_daemon::protocol::manager_v1::ManagerAppConfigRecord {
            uid: 10_000,
            mode: 20,
            permissive: true,
        },
    ];

    let payload =
        handle_read_only_command(ManagerCommand::GetAppCfg, &state).expect("app cfg succeeds");
    assert_eq!(payload.len(), 12);

    let set_payload = encode_app_config(&[ManagerAppConfigRecord {
        uid: 10_000,
        mode: 30,
        permissive: false,
    }]);
    let frame = manager_frame(ManagerCommand::SetAppCfg, &set_payload);
    let response = handle_manager_command(&frame, &mut state, |payload| {
        assert!(String::from_utf8_lossy(payload).contains("10000uid10000"));
        Ok(true)
    })
    .expect("set app cfg succeeds");
    assert_eq!(response, b"success");
    assert_eq!(
        state.app_config,
        vec![ManagerAppConfigRecord {
            uid: 10_000,
            mode: 30,
            permissive: false,
        }]
    );
    let info = state.manager_log.render(LogView::Info);
    assert!(info.contains("配置变化"));
    assert!(info.contains("10000uid10000"));
    assert!(info.contains("20->30"));
}

#[test]
fn set_app_label_logs_legacy_label_update_summary() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut state = ReadOnlyState::default();
    let label_path = temp.path().join("applabel.txt");
    state.app_label_path = Some(label_path.to_string_lossy().into_owned());
    let frame = manager_frame(
        ManagerCommand::SetAppLabel,
        "10000 Example App\n10001 Other App\n".as_bytes(),
    );

    let response =
        handle_manager_command(&frame, &mut state, |_| Ok(true)).expect("set app label succeeds");

    assert_eq!(response, b"success");
    let info = state.manager_log.render(LogView::Info);
    assert!(info.contains("更新 2 款应用名称"));
    assert!(info.contains("[Example App]"));
    assert!(info.contains("[Other App]"));
    assert_eq!(
        fs::read_to_string(label_path).unwrap(),
        "10000 Example App\n10001 Other App\n"
    );
}

#[test]
fn set_app_cfg_restores_file_state_and_hook_when_new_hook_sync_fails() {
    let temp = tempfile::tempdir().expect("tempdir");
    let app_config_path = temp.path().join("appcfg.txt");
    fs::write(&app_config_path, "10000uid10000 20 1\n").expect("seed config");
    let mut state = ReadOnlyState::default();
    state.app_config_path = Some(app_config_path.to_string_lossy().into_owned());
    state.app_config = vec![ManagerAppConfigRecord {
        uid: 10_000,
        mode: 20,
        permissive: true,
    }];
    let frame = manager_frame(
        ManagerCommand::SetAppCfg,
        &encode_app_config(&[ManagerAppConfigRecord {
            uid: 10_000,
            mode: 30,
            permissive: false,
        }]),
    );
    let mut hook_payloads = Vec::new();

    let response = handle_manager_command(&frame, &mut state, |payload| {
        hook_payloads.push(payload.to_vec());
        Ok(hook_payloads.len() != 1)
    })
    .expect("hook rejection is reported without corrupting persistence");

    assert_eq!(response, b"failure");
    assert_eq!(
        fs::read_to_string(&app_config_path).unwrap(),
        "10000uid10000 20 1\n"
    );
    assert_eq!(state.app_config[0].mode, 20);
    assert_eq!(hook_payloads.len(), 2, "old hook config must be restored");
    assert!(!temp.path().join(".appcfg.txt.tmp").exists());
}

#[test]
fn set_app_cfg_noop_preserves_package_names_and_file_bytes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let app_config_path = temp.path().join("appcfg.txt");
    let original = "com.example.first 30 1\ncom.example.second 40 0\n";
    fs::write(&app_config_path, original).expect("seed config");
    let mut state = ReadOnlyState::default();
    state.app_config_path = Some(app_config_path.to_string_lossy().into_owned());
    state.app_config = vec![
        ManagerAppConfigRecord {
            uid: 10_001,
            mode: 30,
            permissive: true,
        },
        ManagerAppConfigRecord {
            uid: 10_002,
            mode: 40,
            permissive: false,
        },
    ];
    let frame = manager_frame(
        ManagerCommand::SetAppCfg,
        &encode_app_config(&state.app_config),
    );

    let response = handle_manager_command(&frame, &mut state, |_| Ok(true))
        .expect("no-op config save succeeds");

    assert_eq!(response, b"success");
    assert_eq!(fs::read_to_string(&app_config_path).unwrap(), original);
}

#[test]
fn set_app_cfg_change_preserves_existing_package_token() {
    let temp = tempfile::tempdir().expect("tempdir");
    let app_config_path = temp.path().join("appcfg.txt");
    fs::write(&app_config_path, "com.example.first 30 1\n").expect("seed config");
    let mut state = ReadOnlyState::default();
    state.app_config_path = Some(app_config_path.to_string_lossy().into_owned());
    state.app_config = vec![ManagerAppConfigRecord {
        uid: 10_001,
        mode: 30,
        permissive: true,
    }];
    let frame = manager_frame(
        ManagerCommand::SetAppCfg,
        &encode_app_config(&[ManagerAppConfigRecord {
            uid: 10_001,
            mode: 20,
            permissive: false,
        }]),
    );

    let response = handle_manager_command(&frame, &mut state, |_| Ok(true))
        .expect("changed config save succeeds");

    assert_eq!(response, b"success");
    assert_eq!(
        fs::read_to_string(app_config_path).unwrap(),
        "com.example.first 20 0\n"
    );
}

#[test]
fn set_app_cfg_noop_preserves_comments_blank_lines_and_spacing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let app_config_path = temp.path().join("appcfg.txt");
    let original = "# user note\n\ncom.example.first   30   1\n";
    fs::write(&app_config_path, original).expect("seed config");
    let mut state = ReadOnlyState::default();
    state.app_config_path = Some(app_config_path.to_string_lossy().into_owned());
    state.app_config = vec![ManagerAppConfigRecord {
        uid: 10_001,
        mode: 30,
        permissive: true,
    }];
    let frame = manager_frame(
        ManagerCommand::SetAppCfg,
        &encode_app_config(&state.app_config),
    );
    #[cfg(unix)]
    let original_inode = fs::metadata(&app_config_path).unwrap().ino();

    handle_manager_command(&frame, &mut state, |_| Ok(true)).expect("no-op config save succeeds");

    assert_eq!(fs::read_to_string(&app_config_path).unwrap(), original);
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&app_config_path).unwrap().ino(),
        original_inode
    );
}

#[test]
fn set_app_cfg_rebuilds_existing_records_when_config_file_is_missing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let app_config_path = temp.path().join("appcfg.txt");
    let mut state = ReadOnlyState::default();
    state.app_config_path = Some(app_config_path.to_string_lossy().into_owned());
    state.app_config = vec![ManagerAppConfigRecord {
        uid: 10_001,
        mode: 30,
        permissive: true,
    }];
    let frame = manager_frame(
        ManagerCommand::SetAppCfg,
        &encode_app_config(&state.app_config),
    );

    handle_manager_command(&frame, &mut state, |_| Ok(true)).expect("missing config is rebuilt");

    assert_eq!(
        fs::read_to_string(app_config_path).unwrap(),
        "10001uid10001 30 1\n"
    );
}

#[test]
fn write_commands_do_not_report_success_without_persistence_paths() {
    let mut state = ReadOnlyState::default();
    let app_cfg = manager_frame(
        ManagerCommand::SetAppCfg,
        &encode_app_config(&[ManagerAppConfigRecord {
            uid: 10_000,
            mode: 30,
            permissive: false,
        }]),
    );
    assert!(handle_manager_command(&app_cfg, &mut state, |_| Ok(true)).is_err());

    let labels = manager_frame(ManagerCommand::SetAppLabel, b"10000 Example App\n");
    assert!(handle_manager_command(&labels, &mut state, |_| Ok(true)).is_err());

    let settings = manager_frame(ManagerCommand::SetSettingsVar, &[13, 0]);
    let response = handle_manager_command(&settings, &mut state, |_| Ok(true)).unwrap();
    assert_ne!(response, b"success");
}

#[test]
fn empty_app_config_still_returns_legacy_placeholder_record() {
    let state = ReadOnlyState::default();

    let payload =
        handle_read_only_command(ManagerCommand::GetAppCfg, &state).expect("app cfg succeeds");

    assert_eq!(payload.len(), 12);
}

#[test]
fn set_app_config_reports_failure_when_hook_rejects_payload() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut state = ReadOnlyState::default();
    state.app_config_path = Some(
        temp.path()
            .join("appcfg.txt")
            .to_string_lossy()
            .into_owned(),
    );
    let payload = encode_app_config(&[ManagerAppConfigRecord {
        uid: 10_000,
        mode: 30,
        permissive: false,
    }]);
    let frame = manager_frame(ManagerCommand::SetAppCfg, &payload);

    let response =
        handle_manager_command(&frame, &mut state, |_| Ok(false)).expect("set app cfg handled");

    assert_eq!(response, b"failure");
}

#[test]
fn set_settings_var_updates_legacy_setting_byte() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut state = ReadOnlyState::default();
    state.settings_path = Some(
        temp.path()
            .join("settings.db")
            .to_string_lossy()
            .into_owned(),
    );
    let frame = manager_frame(ManagerCommand::SetSettingsVar, &[13, 0]);

    let response = handle_manager_command(&frame, &mut state, |_| Ok(true))
        .expect("set settings var succeeds");

    assert_eq!(response, b"success");
    assert_eq!(state.settings[13], 0);
    let info = state.manager_log.render(LogView::Info);
    assert!(info.ends_with("]  ⚙️设置成功\n"));
    assert!(!info.contains("[13]:0"));
}

#[test]
fn set_settings_var_rejects_invalid_switch_value() {
    let mut state = ReadOnlyState::default();
    let original = state.settings[13];
    let frame = manager_frame(ManagerCommand::SetSettingsVar, &[13, 2]);

    let response =
        handle_manager_command(&frame, &mut state, |_| Ok(true)).expect("set settings var handled");

    let text = String::from_utf8(response).expect("response text");
    assert!(text.contains("开关值错误"));
    assert_eq!(state.settings[13], original);
}

#[test]
fn get_log_filters_in_daemon_using_setting_byte_30() {
    let mut state = ReadOnlyState::default();
    state.manager_log.clear();
    state
        .manager_log
        .push(LogRecord::legacy_text(1_000, "🧊冻结 demo"));
    state.manager_log.push(LogRecord::diagnostic(
        LogLevel::Debug,
        2_000,
        "daemon active: apps=1",
    ));

    state.settings[30] = 0;
    let info = handle_read_only_command(ManagerCommand::GetLog, &state).unwrap();
    assert_eq!(
        String::from_utf8(info).unwrap(),
        "[08:00:01]  🧊冻结 demo\n"
    );

    state.settings[30] = 1;
    let debug = handle_read_only_command(ManagerCommand::GetLog, &state).unwrap();
    assert!(String::from_utf8(debug)
        .unwrap()
        .contains("daemon active: apps=1"));
}

#[test]
fn setting_index_30_accepts_only_log_view_values() {
    for value in 0..=4 {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut state = ReadOnlyState::default();
        state.settings_path = Some(
            temp.path()
                .join("settings.db")
                .to_string_lossy()
                .into_owned(),
        );
        let response = handle_manager_command(
            &manager_frame(ManagerCommand::SetSettingsVar, &[30, value]),
            &mut state,
            |_| Ok(true),
        )
        .expect("log level setting is handled");
        assert_eq!(response, b"success", "value {value} must be accepted");
        assert_eq!(state.settings[30], value);
    }

    let mut state = ReadOnlyState::default();
    let response = handle_manager_command(
        &manager_frame(ManagerCommand::SetSettingsVar, &[30, 5]),
        &mut state,
        |_| Ok(true),
    )
    .expect("invalid log level is handled");
    assert_ne!(response, b"success");
    assert_eq!(state.settings[30], 0);
}

#[test]
fn get_uid_time_returns_managed_legacy_cpu_records_with_delta() {
    let temp = tempfile::tempdir().expect("tempdir");
    let uid_time_path = temp.path().join("show_uid_stat");
    fs::write(
        &uid_time_path,
        "10042: 1000 2000\n10043: 9000 9000\n10044: 0 0\n",
    )
    .expect("write uid cputime");

    let mut state = ReadOnlyState::default();
    state.uid_time_path = uid_time_path.to_string_lossy().into_owned();
    state.app_config = vec![
        ManagerAppConfigRecord {
            uid: 10042,
            mode: 30,
            permissive: false,
        },
        ManagerAppConfigRecord {
            uid: 10043,
            mode: 40,
            permissive: false,
        },
    ];

    let first = handle_manager_command(
        &manager_frame(ManagerCommand::GetUidTime, &[]),
        &mut state,
        |_| Ok(true),
    )
    .expect("uid time succeeds");
    assert_eq!(first.len(), 12);
    assert_eq!(
        first
            .chunks_exact(4)
            .map(|chunk| i32::from_le_bytes(chunk.try_into().unwrap()))
            .collect::<Vec<_>>(),
        vec![10042, 3, 3]
    );

    fs::write(&uid_time_path, "10042: 4000 4000\n10043: 9000 9000\n").expect("update uid cputime");
    let second = handle_manager_command(
        &manager_frame(ManagerCommand::GetUidTime, &[]),
        &mut state,
        |_| Ok(true),
    )
    .expect("uid time succeeds");
    assert_eq!(
        second
            .chunks_exact(4)
            .map(|chunk| i32::from_le_bytes(chunk.try_into().unwrap()))
            .collect::<Vec<_>>(),
        vec![10042, 5, 8]
    );
}

#[test]
fn realtime_info_returns_legacy_image_and_23_int_payload() {
    let mut state = ReadOnlyState::default();
    let mut request = Vec::new();
    request.extend_from_slice(&20_u32.to_le_bytes());
    request.extend_from_slice(&24_u32.to_le_bytes());
    request.extend_from_slice(&123_u32.to_le_bytes());
    let frame = manager_frame(ManagerCommand::GetRealTimeInfo, &request);

    let response =
        handle_manager_command(&frame, &mut state, |_| Ok(true)).expect("realtime info succeeds");

    let image_bytes = 20 * 24 * 4;
    assert_eq!(response.len(), image_bytes + 23 * 4);
    assert!(
        response[..image_bytes].iter().any(|byte| *byte != 0),
        "legacy manager chart image must not be blank"
    );
    assert_eq!(
        i32::from_le_bytes(
            response[image_bytes + 4..image_bytes + 8]
                .try_into()
                .unwrap()
        ),
        123
    );
}

#[test]
fn remaining_legacy_commands_return_compatibility_payloads() {
    let mut state = ReadOnlyState::default();
    state.changelog = "### local changelog\ncompatibility fixes".to_owned();
    state.operation_log_json = "{\"operations\":[{\"action\":\"freeze\"}]}".to_owned();

    let changelog = handle_manager_command(
        &manager_frame(ManagerCommand::GetChangelog, &[]),
        &mut state,
        |_| Ok(true),
    )
    .expect("changelog succeeds");
    assert!(String::from_utf8(changelog)
        .unwrap()
        .contains("local changelog"));

    let uid_time = handle_manager_command(
        &manager_frame(ManagerCommand::GetUidTime, &[]),
        &mut state,
        |_| Ok(true),
    )
    .expect("uid time succeeds");
    assert_eq!(uid_time.len() % 12, 0);

    let proc_state = handle_manager_command(
        &manager_frame(ManagerCommand::GetProcState, &[]),
        &mut state,
        |_| Ok(true),
    )
    .expect("proc state succeeds");
    let proc_state = String::from_utf8(proc_state).unwrap();
    assert!(proc_state.contains("进程冻结状态"));
    assert!(proc_state.contains("后台很干净，一个黑名单应用都没有"));
    assert!(!proc_state.contains("process state:"));

    let cleared = handle_manager_command(
        &manager_frame(ManagerCommand::ClearLog, &[]),
        &mut state,
        |_| Ok(true),
    )
    .expect("clear log succeeds");
    assert!(cleared.is_empty());
    assert!(state.manager_log.is_empty());

    let diagnostics = handle_manager_command(
        &manager_frame(ManagerCommand::GetOperationLogJson, &[]),
        &mut state,
        |_| Ok(true),
    )
    .expect("structured diagnostics still available");
    assert_eq!(diagnostics, b"{\"operations\":[{\"action\":\"freeze\"}]}");
}

#[test]
fn get_freeze_status_returns_manager_five_int_rows() {
    let mut state = ReadOnlyState::default();
    state.freeze_status = vec![
        freezeit_daemon::protocol::manager_v1::ManagerFreezeStatusRecord {
            uid: 10_042,
            foreground: true,
            state: 1,
            seconds: 12,
            process_count: 2,
        },
        freezeit_daemon::protocol::manager_v1::ManagerFreezeStatusRecord {
            uid: 10_043,
            foreground: false,
            state: 3,
            seconds: 34,
            process_count: 1,
        },
    ];

    let payload = handle_manager_command(
        &manager_frame(ManagerCommand::GetFreezeStatus, &[]),
        &mut state,
        |_| Ok(true),
    )
    .expect("freeze status succeeds");

    assert_eq!(payload.len(), 40);
    let values = payload
        .chunks_exact(4)
        .map(|bytes| i32::from_le_bytes(bytes.try_into().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(values, vec![10_042, 1, 1, 12, 2, 10_043, 0, 3, 34, 1]);
}

#[test]
fn get_log_includes_original_emoji_operation_entries() {
    let mut state = ReadOnlyState::default();
    state.manager_log.push(LogRecord::operation(
        freezeit_daemon::domain::operation::ControlOperation {
            operation_id: 7,
            timestamp_ms: 42,
            package_name: "com.example.app".to_owned(),
            uid: 10_123,
            pid_list: vec![123, 124],
            action: freezeit_daemon::domain::operation::ControlAction::Freeze,
            backend: "cgroup.freeze".to_owned(),
            reason: "delay elapsed".to_owned(),
            result: freezeit_daemon::domain::operation::OperationResult::Success,
            details: "process_count=2".to_owned(),
        },
    ));

    let payload = handle_read_only_command(ManagerCommand::GetLog, &state).expect("log succeeds");
    let text = String::from_utf8(payload).expect("log is utf-8");

    assert_eq!(text, "[08:00:00]  ❄️冻结 com.example.app 2进程\n");
    assert!(!text.contains("UID:"));
    assert!(!text.contains("PID:"));
    assert!(!text.contains("方式:"));
    assert!(!text.contains("结果:"));
    assert!(!text.contains("原因:"));
    assert!(!text.contains("backend="));
    assert!(!text.contains("result="));
    assert!(!text.contains("reason="));
    assert!(!text.contains("operationId="));
    assert!(!text.contains("action=freeze"));
}

#[test]
fn startup_state_loads_legacy_module_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    let module_dir = temp.path();
    fs::write(
        module_dir.join("module.prop"),
        "id=freezeit.test\nname=Freezeit Test\nversion=3.2.0\nversionCode=320\n\
         author=jark006\n",
    )
    .expect("write module.prop");
    fs::write(
        module_dir.join("CHANGELOG.md"),
        "### local changelog\nfixed manager UI",
    )
    .expect("write changelog");
    fs::write(
        module_dir.join("boot.log"),
        "[2026-07-04] 启动冻它\n\
         [2026-07-04] WARNING ROM fingerprint mismatch; continuing startup\n\
         baseline=old-device\n\
         device=current-device\n\
         [2026-07-04] loaded config\n",
    )
    .expect("write log");
    fs::write(module_dir.join("appcfg.txt"), "10000uid10000 31 0\n").expect("write appcfg");
    let mut settings = ReadOnlyState::default().settings;
    settings[13] = 0;
    fs::write(module_dir.join("settings.db"), settings).expect("write settings");

    let state = startup_read_only_state_from_paths(&DaemonPaths::from_module_dir(
        module_dir.display().to_string(),
    ));

    assert_eq!(state.module_id, "freezeit.test");
    assert_eq!(state.module_name, "Freezeit Test");
    assert_eq!(state.version, "3.2.0");
    assert_eq!(state.version_code, 320);
    assert_eq!(state.settings[13], 0);
    assert_eq!(
        state.app_config,
        vec![ManagerAppConfigRecord {
            uid: 10_000,
            mode: 31,
            permissive: false,
        }]
    );
    let debug = state.manager_log.render(LogView::Debug);
    assert!(debug.contains("启动冻它"));
    assert!(debug.contains("loaded config"));
    assert!(debug.contains("daemon active"));
    assert!(!debug.contains("ROM fingerprint mismatch"));
    assert!(state.changelog.contains("local changelog"));
    assert_ne!(state.android_version, "Unknown");
    assert_ne!(state.kernel_version, "Unknown");
}

#[test]
fn startup_log_keeps_verified_cpp_lines_in_info_and_diagnostics_in_debug() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(
        temp.path().join("boot.log"),
        "[12:34:56]  ⚙️设置成功\n\
         daemon active: apps=1\n\
         hook config synced: managed_apps=1 settings=256\n",
    )
    .expect("write boot log");
    let mut state = startup_read_only_state_from_paths(&DaemonPaths::from_module_dir(
        temp.path().display().to_string(),
    ));

    state.settings[30] = 0;
    let info = String::from_utf8(
        handle_read_only_command(ManagerCommand::GetLog, &state).expect("INFO log"),
    )
    .expect("INFO is UTF-8");
    assert_eq!(info, "[12:34:56]  ⚙️设置成功\n");
    assert!(!info.contains("daemon active:"));
    assert!(!info.contains("hook config synced:"));
    assert!(!info.contains("[INFO]"));

    state.settings[30] = 1;
    let debug = String::from_utf8(
        handle_read_only_command(ManagerCommand::GetLog, &state).expect("DEBUG log"),
    )
    .expect("DEBUG is UTF-8");
    assert!(debug.contains("[12:34:56]  ⚙️设置成功"));
    assert!(debug.contains("daemon active: apps=1"));
    assert!(debug.contains("hook config synced: managed_apps=1 settings=256"));
}

#[test]
fn home_status_card_does_not_call_unsupported_legacy_health_command() {
    let home = include_str!(
        "../../../freezeitApp/app/src/main/java/io/github/jark006/freezeit/fragment/Home.java"
    );

    assert!(
        !home.contains("Utils.freezeitTask(ManagerCmd.getHealthReport"),
        "legacy self-use releases ship the C++ daemon, which does not implement command 71"
    );
    assert!(
        home.contains("hasHealthStatus") && home.contains("StaticData.workMode"),
        "home status must keep a legacy fallback when getPropInfo has no daemon health fields"
    );
}

#[test]
fn home_realtime_card_shows_loading_state_before_first_sample() {
    let layout = include_str!("../../../freezeitApp/app/src/main/res/layout/fragment_home.xml");
    let strings = include_str!("../../../freezeitApp/app/src/main/res/values-zh/strings.xml");
    let cpu_id = layout
        .find("android:id=\"@+id/cpu\"")
        .expect("realtime CPU status text exists");
    let cpu_tail = &layout[cpu_id..cpu_id + 500.min(layout.len() - cpu_id)];

    assert!(
        cpu_tail.contains("android:text=\"@string/realtime_loading\""),
        "home realtime card must explain the initial empty state while the first sample loads"
    );
    assert!(
        strings.contains("<string name=\"realtime_loading\">"),
        "Chinese resources must provide the realtime loading message"
    );
}

#[test]
fn home_health_status_uses_localized_resources() {
    let home = include_str!(
        "../../../freezeitApp/app/src/main/java/io/github/jark006/freezeit/fragment/Home.java"
    );
    let strings = include_str!("../../../freezeitApp/app/src/main/res/values-zh/strings.xml");

    assert!(
        !home.contains("setText(\"Daemon \" +"),
        "home health status must not hard-code English text in the Chinese UI"
    );
    assert!(
        home.contains("localizedHealthStatus")
            && home.contains("R.string.health_status_format")
            && home.contains("R.string.health_status_active")
            && home.contains("R.string.health_status_degraded")
            && home.contains("R.string.health_status_inactive")
            && home.contains("R.string.health_status_unknown"),
        "home health values must map daemon status tokens through localized resources"
    );
    for resource in [
        "health_status_format",
        "health_status_active",
        "health_status_degraded",
        "health_status_inactive",
        "health_status_unknown",
    ] {
        assert!(
            strings.contains(&format!("<string name=\"{resource}\">")),
            "Chinese resources must define {resource}"
        );
    }
}

#[test]
fn home_realtime_initialization_handles_already_laid_out_image() {
    let home = include_str!(
        "../../../freezeitApp/app/src/main/java/io/github/jark006/freezeit/fragment/Home.java"
    );

    assert!(
        home.contains("binding.cpuImg.getWidth() > 0")
            && home.contains("binding.cpuImg.getHeight() > 0")
            && home.contains("initializeRealTimeDimensions()"),
        "home must initialize realtime dimensions immediately when the image is already laid out"
    );
}

#[test]
fn home_version_values_are_width_constrained() {
    let layout = include_str!("../../../freezeitApp/app/src/main/res/layout/fragment_home.xml");
    let kernel_id = layout
        .find("android:id=\"@+id/kernel_ver\"")
        .expect("kernel version text exists");
    let kernel_tail = &layout[kernel_id..kernel_id + 500.min(layout.len() - kernel_id)];

    assert!(kernel_tail.contains("android:layout_width=\"0dp\""));
    assert!(kernel_tail.contains("android:layout_weight="));
    assert!(kernel_tail.contains("android:ellipsize=\"end\""));
}

#[test]
fn logcat_display_scroll_area_fills_viewport() {
    let layout = include_str!("../../../freezeitApp/app/src/main/res/layout/fragment_logcat.xml");
    let logcat = include_str!(
        "../../../freezeitApp/app/src/main/java/io/github/jark006/freezeit/fragment/Logcat.java"
    );

    assert!(
        layout.contains("android:fillViewport=\"true\""),
        "log page scroll area must fill available viewport instead of collapsing to text height"
    );
    assert!(
        layout.contains("android:paddingBottom=\"@dimen/fab_margin\""),
        "log page needs bottom padding so floating action buttons do not cover tail logs"
    );
    assert!(
        logcat.contains("scrollLogToBottom")
            && logcat.contains("binding.logView.scrollTo(0, Math.max(scrollAmount, 0))"),
        "log page must scroll the TextView itself because ScrollingMovementMethod owns log scrolling"
    );
    assert!(
        !logcat.contains("fullScroll(View.FOCUS_DOWN)"),
        "outer ScrollView fullScroll does not reach the TextView's internal scroll position"
    );
}

#[test]
fn logcat_clear_requires_explicit_confirmation() {
    let logcat = include_str!(
        "../../../freezeitApp/app/src/main/java/io/github/jark006/freezeit/fragment/Logcat.java"
    );
    let clear_listener = logcat
        .find("binding.fabClear.setOnClickListener")
        .expect("clear listener exists");
    let clear_listener = &logcat[clear_listener..];

    assert!(
        clear_listener.contains("new AlertDialog.Builder(requireContext())"),
        "clear log must show an in-app confirmation dialog"
    );
    assert!(
        clear_listener.contains("setNegativeButton(android.R.string.cancel, null)"),
        "clear log confirmation must provide a non-destructive cancel action"
    );
    assert!(
        clear_listener.contains("setPositiveButton(R.string.clear_log_text"),
        "clear command must only run from the explicit confirmation action"
    );
}

#[test]
fn logcat_switches_between_work_log_and_xposed_log_not_json_diagnostics() {
    let logcat = include_str!(
        "../../../freezeitApp/app/src/main/java/io/github/jark006/freezeit/fragment/Logcat.java"
    );

    assert!(logcat.contains("isGetWorkLog ? ManagerCmd.getLog : ManagerCmd.getXpLog"));
    assert!(!logcat.contains("isGetWorkLog ? ManagerCmd.getLog : ManagerCmd.getOperationLogJson"));
    let reset_timer_start = logcat
        .find("void resetTimer()")
        .expect("resetTimer method exists");
    let reset_timer_body = &logcat[reset_timer_start..reset_timer_start + 260];
    assert!(
        reset_timer_body.contains("lastLogLen = 0;"),
        "switching log sources must force refresh even when payload byte lengths match"
    );
}

#[test]
fn xposed_config_payload_translates_manager_binary_records() {
    let settings = [1_u8, 0, 30];
    let payload = encode_app_config(&[
        ManagerAppConfigRecord {
            uid: 10_000,
            mode: 30,
            permissive: true,
        },
        ManagerAppConfigRecord {
            uid: 10_001,
            mode: 40,
            permissive: false,
        },
    ]);

    let text = String::from_utf8(encode_xposed_config_payload(&settings, &payload).unwrap())
        .expect("xposed config is utf-8");

    assert_eq!(text, "1 0 30\n10000uid10000\n10000");
}

#[test]
fn xposed_config_payload_keeps_empty_config_parseable_by_hook_split() {
    let settings = [1_u8, 0, 30];
    let payload = encode_app_config(&[ManagerAppConfigRecord {
        uid: 10_001,
        mode: 40,
        permissive: false,
    }]);

    let text = String::from_utf8(encode_xposed_config_payload(&settings, &payload).unwrap())
        .expect("xposed config is utf-8");

    assert_eq!(text, "1 0 30\n \n ");
}

#[test]
fn missing_hook_evaluates_degraded_and_blocks_control() {
    let health = ModuleHealth::evaluate(true, true, false, true, true, true);

    assert_eq!(health.status, HealthStatus::Degraded);
    assert!(!health.is_safe_for_control());
    assert!(health
        .degraded_reasons
        .iter()
        .any(|reason| reason.contains("hook")));
}

#[test]
fn missing_hook_bridge_classifies_as_fail_closed_degraded_health() {
    let bridge = classify_bridge_error(&DaemonError::system("Connection refused"));
    assert!(matches!(bridge, HookBridgeStatus::Missing(_)));
    assert!(!bridge.is_ready_for_control());

    let health = ModuleHealth::with_hook_bridge(
        true,
        true,
        true,
        true,
        true,
        bridge.is_ready_for_control(),
        Some(format!("hook bridge {}", bridge.health_label())),
    );

    assert_eq!(health.status, HealthStatus::Degraded);
    assert!(!health.is_safe_for_control());
}

#[test]
fn capability_failures_are_reported_as_degraded_reasons() {
    let health = ModuleHealth::with_capability_failures(
        true, true, true, true, false, false, false, false, false,
    );

    assert_eq!(health.status, HealthStatus::Degraded);
    assert!(health
        .degraded_reasons
        .iter()
        .any(|reason| reason.contains("package inventory")));
    assert!(health
        .degraded_reasons
        .iter()
        .any(|reason| reason.contains("freezer")));
    assert!(health
        .degraded_reasons
        .iter()
        .any(|reason| reason.contains("network")));
    assert!(health
        .degraded_reasons
        .iter()
        .any(|reason| reason.contains("wake-lock")));
    assert!(health
        .degraded_reasons
        .iter()
        .any(|reason| reason.contains("screen-state")));
}

#[test]
fn v2_diagnostic_commands_return_json_payloads() {
    let diagnostics = DiagnosticState {
        health: ModuleHealth::evaluate(true, true, false, true, true, true),
        capabilities: vec![ControlCapability::missing(
            CapabilityName::LsposedSystemServer,
            "hook missing",
        )],
        operation_log: OperationLog::new(8),
    };
    let state = read_only_state_with_diagnostics(&diagnostics);

    assert!(String::from_utf8(
        handle_read_only_command(ManagerCommand::GetHealthReport, &state).unwrap()
    )
    .unwrap()
    .contains("\"status\":\"degraded\""));
    assert!(String::from_utf8(
        handle_read_only_command(ManagerCommand::GetCapabilityReport, &state).unwrap()
    )
    .unwrap()
    .contains("\"capabilities\""));
    assert!(String::from_utf8(
        handle_read_only_command(ManagerCommand::GetOperationLogJson, &state).unwrap()
    )
    .unwrap()
    .contains("\"operations\""));
    assert!(String::from_utf8(
        handle_read_only_command(ManagerCommand::RunSelfCheck, &state).unwrap()
    )
    .unwrap()
    .contains("\"controlAllowed\":false"));
}

#[test]
fn v2_diagnostic_command_ids_match_published_contract() {
    assert_eq!(
        ManagerCommand::try_from(71).unwrap(),
        ManagerCommand::GetHealthReport
    );
    assert_eq!(
        ManagerCommand::try_from(72).unwrap(),
        ManagerCommand::GetCapabilityReport
    );
    assert_eq!(
        ManagerCommand::try_from(73).unwrap(),
        ManagerCommand::GetCompatibilityBaseline
    );
    assert_eq!(
        ManagerCommand::try_from(74).unwrap(),
        ManagerCommand::GetOperationLogJson
    );
    assert_eq!(
        ManagerCommand::try_from(75).unwrap(),
        ManagerCommand::RunSelfCheck
    );
    assert!(ManagerCommand::try_from(70).is_err());
}

#[test]
fn v2_compatibility_baseline_command_returns_report_json() {
    let mut state = ReadOnlyState::default();
    state.compatibility_report_json = RuntimeEnvironment::new(
        "CPH2653",
        "16",
        36,
        "runtime-fingerprint",
        "6.6.89",
        true,
        true,
        true,
    )
    .compatibility_json(&[]);

    let text = String::from_utf8(
        handle_read_only_command(ManagerCommand::GetCompatibilityBaseline, &state).unwrap(),
    )
    .unwrap();

    assert!(text.contains("\"deviceModel\":\"CPH2653\""));
    assert!(text.contains("\"androidVersion\":\"16\""));
    assert!(text.contains("\"capabilities\""));
}

#[test]
fn startup_loaded_config_can_be_synchronized_to_hook_without_manager_save() {
    let mut state = ReadOnlyState::default();
    state.app_config = vec![ManagerAppConfigRecord {
        uid: 10_123,
        mode: 30,
        permissive: true,
    }];

    let response = sync_loaded_config_to_hook(&mut state, |payload| {
        let text = String::from_utf8_lossy(payload);
        assert!(text.contains("10123uid10123"));
        assert!(text.lines().nth(2).unwrap_or_default().contains("10123"));
        Ok(true)
    })
    .expect("startup sync succeeds");

    assert!(response);
    assert!(state
        .manager_log
        .render(LogView::Debug)
        .contains("hook config synced"));
}

#[test]
fn settings_log_level_selector_replaces_debug_switch_without_initial_write() {
    let settings = include_str!(
        "../../../freezeitApp/app/src/main/java/io/github/jark006/freezeit/activity/Settings.java"
    );
    let layout = include_str!("../../../freezeitApp/app/src/main/res/layout/activity_settings.xml");
    let arrays = include_str!("../../../freezeitApp/app/src/main/res/values/arrays.xml");

    assert!(layout.contains("android:id=\"@+id/log_level_title\""));
    assert!(layout.contains("android:id=\"@+id/log_level_spinner\""));
    assert!(layout.contains("android:entries=\"@array/log_levels\""));
    assert!(!layout.contains("android:id=\"@+id/switch_debug\""));
    assert!(arrays.contains(
        "<string-array name=\"log_levels\">\n        <item>INFO</item>\n        <item>WARN</item>\n        <item>ERROR</item>\n        <item>CRITICAL</item>\n        <item>DEBUG</item>"
    ));

    assert!(settings.contains("logLevelSpinner;"));
    assert!(settings.contains("void InitLogLevelSpinner()"));
    assert!(settings.contains("LogLevelCodec.toSpinnerPosition"));
    assert!(settings.contains("LogLevelCodec.toStorageValue"));
    assert!(settings.contains("InitLogLevelSpinner();"));
    assert!(!settings.contains("debugSwitch"));

    let init = settings
        .split("void InitLogLevelSpinner()")
        .nth(1)
        .expect("log level initializer exists");
    assert!(
        init.find("setSelection").unwrap() < init.find("setOnItemSelectedListener").unwrap(),
        "the current value must be selected before the listener is attached"
    );
}

#[test]
fn failed_log_level_write_restores_persisted_selection() {
    let settings = include_str!(
        "../../../freezeitApp/app/src/main/java/io/github/jark006/freezeit/activity/Settings.java"
    );
    let failure_handler = settings
        .split("case SET_VAR_FAIL:")
        .nth(1)
        .expect("failure handler exists");

    assert!(failure_handler.contains("msg.arg1 == debugIdx"));
    assert!(failure_handler.contains("logLevelSpinner.setSelection"));
    assert!(failure_handler.contains("LogLevelCodec.toSpinnerPosition"));
}

#[test]
fn settings_write_completion_keeps_its_own_index_and_value() {
    let settings = include_str!(
        "../../../freezeitApp/app/src/main/java/io/github/jark006/freezeit/activity/Settings.java"
    );

    assert!(settings.contains("void setVarTask(int index, int value)"));
    assert!(settings.contains("msg.arg1 = index"));
    assert!(settings.contains("msg.arg2 = value"));
    assert!(settings.contains("settingsVar[msg.arg1] = (byte) msg.arg2"));
    assert!(!settings.contains("varIndexForHandle"));
    assert!(!settings.contains("newValueForHandle"));
}

#[test]
fn module_upgrade_never_uninstalls_manager_or_clears_app_data() {
    let customize = include_str!("../../../magisk/customize.sh");

    assert!(customize.contains("pm install -r -f \"$apkPath\""));
    assert!(!customize.contains("pm uninstall io.github.jark006.freezeit"));
    assert!(!customize.contains("pm clear io.github.jark006.freezeit"));
}

#[test]
fn control_loop_merges_operations_without_replacing_manager_log_snapshot() {
    let socket_source = include_str!("../../src/sys/socket.rs");

    assert!(!socket_source.contains("state.manager_log = state_snapshot.manager_log"));
    assert!(socket_source.contains("state.manager_log.push(LogRecord::operation(operation))"));
}
