use freezeit_daemon::app::download_deferral::{
    parse_uid_rx_bytes, DownloadDeferral, DownloadDeferralAction, UidRxSample,
    DOWNLOAD_THRESHOLD_BYTES_PER_SEC,
};
use freezeit_daemon::{
    app::controller::{run_control_pass_with_sampling, RuntimeControlState},
    domain::runtime::{ControlState, ProcessState, RuntimeProcess},
    protocol::manager_v1::ManagerAppConfigRecord,
};

#[test]
fn parses_android_16_netstats_uid_rx_bytes() {
    let dump = r#"
      mAppUidStatsMap: OK
      mAppUidStatsMap:
        uid rxBytes rxPackets txBytes txPackets
        10502 131259371 87762 796372 15107
        10140 489026 831 288329 842
      mStatsMapA:
    "#;

    let values = parse_uid_rx_bytes(dump).expect("netstats table parses");
    assert_eq!(values.get(&10_502), Some(&131_259_371));
    assert_eq!(values.get(&10_140), Some(&489_026));
}

#[test]
fn active_cloud_download_postpones_freeze() {
    let mut deferral = DownloadDeferral::default();
    assert_eq!(
        deferral.evaluate(
            10_123,
            "com.baidu.netdisk",
            UidRxSample::Value(1_000),
            1_000,
        ),
        DownloadDeferralAction::Postpone
    );
    assert_eq!(
        deferral.evaluate(
            10_123,
            "com.baidu.netdisk",
            UidRxSample::Value(1_000 + DOWNLOAD_THRESHOLD_BYTES_PER_SEC + 1),
            2_000,
        ),
        DownloadDeferralAction::Postpone
    );
}

#[test]
fn threshold_is_strictly_greater_than_five_mib_per_second() {
    let mut deferral = DownloadDeferral::default();
    deferral.evaluate(
        10_124,
        "com.quark.clouddrive",
        UidRxSample::Value(2_000),
        1_000,
    );
    assert_eq!(
        deferral.evaluate(
            10_124,
            "com.quark.clouddrive",
            UidRxSample::Value(2_000 + DOWNLOAD_THRESHOLD_BYTES_PER_SEC),
            2_000,
        ),
        DownloadDeferralAction::Proceed
    );
}

#[test]
fn sampling_failure_is_fail_safe_for_candidate_packages() {
    let mut deferral = DownloadDeferral::default();
    assert_eq!(
        deferral.evaluate(10_125, "com.pikpak.android", UidRxSample::Failed, 1_000,),
        DownloadDeferralAction::Postpone
    );
    assert_eq!(
        deferral.evaluate(10_126, "com.example.reader", UidRxSample::Failed, 1_000,),
        DownloadDeferralAction::Proceed
    );
}

#[test]
fn control_pass_records_postpone_while_download_is_active() {
    let config = [ManagerAppConfigRecord {
        uid: 10_123,
        mode: 30,
        permissive: false,
    }];
    let process = RuntimeProcess {
        pid: 123,
        uid: 10_123,
        package_name: "com.baidu.netdisk".to_owned(),
        process_name: "com.baidu.netdisk".to_owned(),
        proc_state: ProcessState::Cached,
        control_state: ControlState::Running,
        cgroup_freeze_path: Some("/sys/fs/cgroup/uid_10123/cgroup.freeze".to_owned()),
        binder_state: Some("idle".to_owned()),
        start_time_ticks: Some(1),
        last_seen_at_ms: 0,
    };
    let mut state = RuntimeControlState::default();
    let mut sample = 1_000;
    let mut freezes = 0;

    for timestamp_ms in [1_000, 2_000] {
        run_control_pass_with_sampling(
            &mut state,
            &config,
            &[],
            |_, _| Ok(vec![process.clone()]),
            |_| {
                freezes += 1;
                Ok(())
            },
            |_| Ok(()),
            |_| Ok(true),
            |_, _| {
                let value = sample;
                sample += DOWNLOAD_THRESHOLD_BYTES_PER_SEC + 1;
                Ok(Some(value))
            },
            &[],
            timestamp_ms,
        )
        .unwrap();
    }

    assert_eq!(freezes, 0);
    assert!(state.operation_log.to_json().contains("download activity"));
}

#[test]
fn non_candidate_app_never_invokes_netstats_sampler() {
    let config = [ManagerAppConfigRecord {
        uid: 10_200,
        mode: 30,
        permissive: false,
    }];
    let process = RuntimeProcess {
        pid: 200,
        uid: 10_200,
        package_name: "com.example.reader".to_owned(),
        process_name: "com.example.reader".to_owned(),
        proc_state: ProcessState::Cached,
        control_state: ControlState::Running,
        cgroup_freeze_path: Some("/sys/fs/cgroup/uid_10200/cgroup.freeze".to_owned()),
        binder_state: Some("idle".to_owned()),
        start_time_ticks: Some(1),
        last_seen_at_ms: 0,
    };
    let mut state = RuntimeControlState::default();
    let mut samples = 0;

    run_control_pass_with_sampling(
        &mut state,
        &config,
        &[],
        |_, _| Ok(vec![process.clone()]),
        |_| Ok(()),
        |_| Ok(()),
        |_| Ok(true),
        |_, _| {
            samples += 1;
            Ok(Some(1))
        },
        &[],
        1_000,
    )
    .unwrap();

    assert_eq!(samples, 0);
}
