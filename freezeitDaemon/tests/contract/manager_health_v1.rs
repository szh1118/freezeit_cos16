use freezeit_daemon::{
    app::compatibility::RuntimeEnvironment,
    app::controller::{
        read_only_state_with_diagnostics, startup_read_only_state_from_paths,
        sync_loaded_config_to_hook, DiagnosticState,
    },
    app::error::DaemonError,
    app::health::{HealthStatus, ModuleHealth},
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
    assert!(state.log.contains("配置变化"));
    assert!(state.log.contains("10000uid10000"));
    assert!(state.log.contains("20->30"));
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
    assert!(state.log.contains("更新 2 款应用名称"));
    assert!(state.log.contains("[Example App]"));
    assert!(state.log.contains("[Other App]"));
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
    state.log = "daemon starting\noperation log line\n".to_owned();
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
    assert_eq!(cleared, b"\n");
    assert_eq!(state.log, "\n");

    let diagnostics = handle_manager_command(
        &manager_frame(ManagerCommand::GetOperationLogJson, &[]),
        &mut state,
        |_| Ok(true),
    )
    .expect("structured diagnostics still available");
    assert_eq!(diagnostics, b"{\"operations\":[{\"action\":\"freeze\"}]}");
}

#[test]
fn get_log_includes_original_emoji_operation_entries() {
    let mut state = ReadOnlyState::default();
    state.log = "daemon starting\n".to_owned();
    let mut operation_log = OperationLog::new(2);
    operation_log.push(freezeit_daemon::domain::operation::ControlOperation {
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
    });
    state.operation_log_text = operation_log.to_legacy_text();

    let payload = handle_read_only_command(ManagerCommand::GetLog, &state).expect("log succeeds");
    let text = String::from_utf8(payload).expect("log is utf-8");

    assert!(text.contains("daemon starting"));
    assert!(text.contains("❄️冻结 com.example.app 2进程"));
    assert!(text.contains("UID:10123"));
    assert!(text.contains("结果:成功"));
    assert!(text.contains("原因:delay elapsed"));
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
    assert!(state.log.contains("启动冻它"));
    assert!(state.log.contains("loaded config"));
    assert!(state.log.contains("daemon active"));
    assert!(!state.log.contains("ROM fingerprint mismatch"));
    assert!(state.changelog.contains("local changelog"));
    assert_ne!(state.android_version, "Unknown");
    assert_ne!(state.kernel_version, "Unknown");
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
    assert!(state.log.contains("hook config synced"));
}
