use std::fs;

use freezeit_daemon::{
    app::compatibility::{load_verified_targets, RuntimeEnvironment, VerifiedTarget},
    domain::capability::{CapabilityName, CapabilityStatus, ControlCapability, RiskLevel},
    protocol::manager_v2::compatibility_report_json,
};

#[test]
fn compatibility_report_separates_target_runtime_and_control_status() {
    let environment = RuntimeEnvironment {
        device_model: "CPH2653".to_owned(),
        android_version: "16".to_owned(),
        sdk: 36,
        fingerprint: "runtime-fingerprint".to_owned(),
        kernel: "6.6.89-android15".to_owned(),
        root_ready: true,
        hook_ready: true,
        freezer_ready: false,
        verified_targets: vec![VerifiedTarget::new("CPH2653", 36)],
    };
    let json = environment.compatibility_json(&[ControlCapability::missing(
        CapabilityName::LsposedSystemServer,
        "hook unavailable",
    )]);

    assert!(json.contains("\"deviceModel\":\"CPH2653\""));
    assert!(json.contains("\"androidVersion\":\"16\""));
    assert!(json.contains("\"sdk\":36"));
    assert!(json.contains("\"schema\":2"));
    assert!(json.contains("\"rootReady\":true"));
    assert!(json.contains("\"hookReady\":true"));
    assert!(json.contains("\"freezerReady\":false"));
    assert!(json.contains("\"verifiedTarget\":true"));
    assert!(json.contains("\"runtimeCompatible\":true"));
    assert!(json.contains("\"controlAllowed\":false"));
    assert!(json.contains("\"limitations\":["));
    assert!(json.contains("hook unavailable"));
}

#[test]
fn compatibility_report_disables_control_when_required_capability_is_missing() {
    let environment = RuntimeEnvironment::new(
        "CPH2653",
        "16",
        36,
        "runtime-fingerprint",
        "6.6.89-android15",
        true,
        true,
        true,
    )
    .with_verified_targets(vec![VerifiedTarget::new("CPH2653", 36)]);

    assert!(!environment.allows_control(&[ControlCapability::missing(
        CapabilityName::CgroupV2Freezer,
        "missing cgroup.freeze",
    )]));
}

#[test]
fn unverified_target_is_fail_closed_even_when_runtime_capabilities_are_ready() {
    let environment = RuntimeEnvironment::new(
        "generic",
        "16",
        36,
        "runtime-fingerprint",
        "6.6.0",
        true,
        true,
        true,
    );

    assert!(!environment.allows_control(&[]));
}

#[test]
fn verified_targets_are_loaded_from_rom_baseline_and_allowlist() {
    let temp = tempfile::tempdir().expect("tempdir");
    let baseline = temp.path().join("rom_baseline.prop");
    let allowlist = temp.path().join("verified_targets.txt");
    fs::write(&baseline, "rom.product=CPH2649IN\nrom.android.version=16\n").expect("baseline");
    fs::write(&allowlist, "CPH2653 sdk=36\n# comment\n").expect("allowlist");

    let targets = load_verified_targets(&baseline, &allowlist).expect("targets");

    assert!(targets.contains(&VerifiedTarget::new("CPH2649IN", 36)));
    assert!(targets.contains(&VerifiedTarget::new("CPH2653", 36)));
}

#[test]
fn manager_v2_exposes_compatibility_report_json() {
    let environment = RuntimeEnvironment::new(
        "generic",
        "16",
        36,
        "runtime-fingerprint",
        "6.6.0",
        true,
        true,
        true,
    );
    let json = compatibility_report_json(
        &environment,
        &[ControlCapability {
            name: CapabilityName::SignalControl,
            status: CapabilityStatus::Available,
            evidence: "kill(2) available".to_owned(),
            checked_at_ms: 0,
            risk_level: RiskLevel::Caution,
        }],
    );

    assert!(json.contains("\"deviceModel\":\"generic\""));
    assert!(json.contains("\"verifiedTarget\":false"));
    assert!(json.contains("\"runtimeCompatible\":true"));
}
