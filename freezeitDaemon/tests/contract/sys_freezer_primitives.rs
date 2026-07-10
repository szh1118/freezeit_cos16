use std::{fs, io};

use freezeit_daemon::{
    domain::capability::CapabilityStatus,
    sys::{
        binder::{
            binder_freezer_ioctl_number, binder_freezer_request,
            detect_binder_freezer_capability_from_candidates,
            detect_binder_freezer_capability_with_probe, BinderFreezeInfo, BinderFreezeRequest,
        },
        cgroup::{
            detect_cgroup_v2_freezer_capability, read_freeze_state, write_freeze_state,
            CgroupFreezerPreference, FreezeState,
        },
    },
};

#[test]
fn cgroup_freeze_state_round_trips_through_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let freeze_file = temp.path().join("cgroup.freeze");
    fs::write(&freeze_file, "0").expect("seed freeze file");

    assert_eq!(
        read_freeze_state(&freeze_file).expect("read"),
        FreezeState::Thawed
    );

    write_freeze_state(&freeze_file, FreezeState::Frozen).expect("write frozen");
    assert_eq!(fs::read_to_string(&freeze_file).expect("read raw"), "1");

    write_freeze_state(&freeze_file, FreezeState::Thawed).expect("write thawed");
    assert_eq!(fs::read_to_string(&freeze_file).expect("read raw"), "0");
}

#[test]
fn binder_freezer_abi_matches_linux_android_uapi() {
    assert_eq!(std::mem::size_of::<BinderFreezeInfo>(), 12);
    assert_eq!(binder_freezer_ioctl_number(), 0x400c_620e);
    assert_eq!(
        binder_freezer_request(123, BinderFreezeRequest::Freeze, 250),
        BinderFreezeInfo {
            pid: 123,
            enable: 1,
            timeout_ms: 250,
        }
    );
    assert_eq!(
        binder_freezer_request(123, BinderFreezeRequest::Unfreeze, 250),
        BinderFreezeInfo {
            pid: 123,
            enable: 0,
            timeout_ms: 250,
        }
    );
}

#[test]
fn cgroup_v2_detection_reads_controllers_and_prefers_android_app_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cgroup_root = temp.path().join("sys/fs/cgroup");
    let app_freeze = cgroup_root.join("apps/uid_10123/pid_123/cgroup.freeze");
    let system_freeze = cgroup_root.join("system/uid_10123/pid_123/cgroup.freeze");
    fs::create_dir_all(app_freeze.parent().expect("app parent")).expect("mkdir app");
    fs::create_dir_all(system_freeze.parent().expect("system parent")).expect("mkdir system");
    fs::write(cgroup_root.join("cgroup.controllers"), "cpu freezer memory").expect("controllers");
    fs::write(&app_freeze, "0").expect("app freeze");
    fs::write(&system_freeze, "0").expect("system freeze");

    let capability = detect_cgroup_v2_freezer_capability(&cgroup_root).expect("detect");

    assert_eq!(capability.status, CapabilityStatus::Available);
    assert_eq!(
        capability.preference,
        CgroupFreezerPreference::AndroidAppCgroupV2
    );
    assert_eq!(capability.freeze_files.first(), Some(&app_freeze));
    assert!(capability.evidence.contains("cgroup.controllers"));
    assert!(capability.evidence.contains("freezer"));
}

#[test]
fn cgroup_v2_detection_accepts_android_freeze_files_when_root_controllers_omit_freezer() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cgroup_root = temp.path().join("sys/fs/cgroup");
    let app_freeze = cgroup_root.join("apps/uid_10123/pid_123/cgroup.freeze");
    fs::create_dir_all(app_freeze.parent().expect("app parent")).expect("mkdir app");
    fs::write(cgroup_root.join("cgroup.controllers"), "cpu memory").expect("controllers");
    fs::write(&app_freeze, "0").expect("app freeze");

    let capability = detect_cgroup_v2_freezer_capability(&cgroup_root).expect("detect");

    assert_eq!(capability.status, CapabilityStatus::Available);
    assert_eq!(
        capability.preference,
        CgroupFreezerPreference::AndroidAppCgroupV2
    );
    assert!(capability.evidence.contains("contains freezer=false"));
}

#[test]
fn binder_device_rejecting_freezer_ioctl_reports_degraded_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let binder = temp.path().join("dev/binder");
    fs::create_dir_all(binder.parent().expect("binder parent")).expect("mkdir");
    fs::write(&binder, "").expect("binder device placeholder");
    let candidates = vec![binder];

    let capability = detect_binder_freezer_capability_from_candidates(&candidates);

    assert_eq!(capability.status, CapabilityStatus::Degraded);
    assert_eq!(capability.device_path.as_deref(), candidates[0].to_str());
    assert!(capability.evidence.contains("ioctl probe failed"));
}

#[test]
fn binder_probe_accepts_kernel_recognition_errors_as_available() {
    let temp = tempfile::tempdir().expect("tempdir");
    let binder = temp.path().join("dev/binder");
    fs::create_dir_all(binder.parent().expect("binder parent")).expect("mkdir");
    fs::write(&binder, "").expect("binder device placeholder");

    let capability = detect_binder_freezer_capability_with_probe(&[binder], |_| {
        Err(io::Error::from_raw_os_error(libc::ESRCH))
    });

    assert_eq!(capability.status, CapabilityStatus::Available);
    assert!(capability.evidence.contains("kernel recognized"));
}
