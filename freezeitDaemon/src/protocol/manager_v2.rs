use crate::{
    app::{
        compatibility::RuntimeEnvironment,
        health::{HealthStatus, ModuleHealth},
        operation_log::OperationLog,
    },
    domain::capability::{CapabilityName, CapabilityStatus, ControlCapability},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagerV2Command {
    GetHealthReport,
    GetCapabilityReport,
    GetCompatibilityBaseline,
    GetOperationLogJson,
    RunSelfCheck,
}

pub fn health_report_json(health: &ModuleHealth) -> String {
    let reasons = health
        .degraded_reasons
        .iter()
        .map(|reason| format!("\"{}\"", escape_json(reason)))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"status\":\"{}\",\"managerReady\":{},\"daemonReady\":{},\"hookReady\":{},\"rootReady\":{},\"freezerReady\":{},\"policyReady\":{},\"degradedReasons\":[{}]}}",
        health_status_name(health.status),
        health.manager_ready,
        health.daemon_ready,
        health.hook_ready,
        health.root_ready,
        health.freezer_ready,
        health.policy_ready,
        reasons
    )
}

pub fn capability_report_json(capabilities: &[ControlCapability]) -> String {
    let capabilities = capabilities
        .iter()
        .map(|capability| {
            format!(
                "{{\"name\":\"{}\",\"status\":\"{}\",\"reason\":\"{}\"}}",
                capability_name(capability.name),
                capability_status_name(capability.status),
                escape_json(&capability.evidence)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"capabilities\":[{capabilities}]}}")
}

pub fn operation_log_json(log: &OperationLog) -> String {
    log.to_json()
}

pub fn self_check_json(health: &ModuleHealth, capabilities: &[ControlCapability]) -> String {
    // This legacy API does not receive the verified target/runtime compatibility
    // evidence that the control loop requires. It must remain fail-closed rather
    // than advertise a capability it cannot prove.
    self_check_json_with_control_allowed(health, capabilities, false)
}

pub fn self_check_json_for_runtime(
    health: &ModuleHealth,
    capabilities: &[ControlCapability],
    environment: &RuntimeEnvironment,
) -> String {
    let control_allowed = health.is_safe_for_control() && environment.allows_control(capabilities);
    self_check_json_with_control_allowed(health, capabilities, control_allowed)
}

fn self_check_json_with_control_allowed(
    health: &ModuleHealth,
    capabilities: &[ControlCapability],
    control_allowed: bool,
) -> String {
    format!(
        "{{\"controlAllowed\":{},\"health\":{},\"capabilities\":{}}}",
        control_allowed,
        health_report_json(health),
        capability_report_json(capabilities)
    )
}

pub fn compatibility_report_json(
    environment: &RuntimeEnvironment,
    capabilities: &[ControlCapability],
) -> String {
    environment.compatibility_json(capabilities)
}

fn health_status_name(status: HealthStatus) -> &'static str {
    match status {
        HealthStatus::Active => "active",
        HealthStatus::Degraded => "degraded",
        HealthStatus::Inactive => "inactive",
    }
}

fn capability_status_name(status: CapabilityStatus) -> &'static str {
    match status {
        CapabilityStatus::Available => "available",
        CapabilityStatus::Missing => "missing",
        CapabilityStatus::Degraded => "degraded",
        CapabilityStatus::Untested => "untested",
    }
}

fn capability_name(name: CapabilityName) -> &'static str {
    match name {
        CapabilityName::Root => "root",
        CapabilityName::PackageInventory => "package_inventory",
        CapabilityName::LsposedSystemServer => "lsposed_system_server",
        CapabilityName::CgroupV2Freezer => "cgroup_v2_freezer",
        CapabilityName::BinderFreezer => "binder_freezer",
        CapabilityName::SignalControl => "signal_control",
        CapabilityName::NetworkBreak => "network_break",
        CapabilityName::WakelockControl => "wakelock_control",
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
            c if (c as u32) < 0x20 => format!("\\u{:04x}", c as u32).chars().collect::<Vec<_>>(),
            other => vec![other],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::compatibility::{RuntimeEnvironment, VerifiedTarget};

    #[test]
    fn legacy_self_check_refuses_to_allow_control_without_runtime_evidence() {
        let health = ModuleHealth::evaluate(true, true, true, true, true, true);

        assert!(self_check_json(&health, &[]).contains("\"controlAllowed\":false"));
    }

    #[test]
    fn runtime_self_check_requires_a_verified_compatible_target() {
        let health = ModuleHealth::evaluate(true, true, true, true, true, true);
        let unverified = RuntimeEnvironment::new(
            "CPH2653",
            "16",
            36,
            "fingerprint",
            "6.6.89",
            true,
            true,
            true,
        );
        let verified = unverified
            .clone()
            .with_verified_targets(vec![VerifiedTarget::new("CPH2653", 36)]);

        assert!(self_check_json_for_runtime(&health, &[], &unverified)
            .contains("\"controlAllowed\":false"));
        assert!(self_check_json_for_runtime(&health, &[], &verified)
            .contains("\"controlAllowed\":true"));
    }
}
