use std::{fs, path::Path};

use crate::{
    app::error::DaemonError,
    domain::capability::{CapabilityName, CapabilityStatus, ControlCapability, RiskLevel},
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VerifiedTarget {
    pub device_model: String,
    pub sdk: u32,
}

impl VerifiedTarget {
    pub fn new(device_model: impl Into<String>, sdk: u32) -> Self {
        Self {
            device_model: device_model.into(),
            sdk,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEnvironment {
    pub device_model: String,
    pub android_version: String,
    pub sdk: u32,
    pub fingerprint: String,
    pub kernel: String,
    pub root_ready: bool,
    pub hook_ready: bool,
    pub freezer_ready: bool,
    pub verified_targets: Vec<VerifiedTarget>,
}

impl RuntimeEnvironment {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        device_model: impl Into<String>,
        android_version: impl Into<String>,
        sdk: u32,
        fingerprint: impl Into<String>,
        kernel: impl Into<String>,
        root_ready: bool,
        hook_ready: bool,
        freezer_ready: bool,
    ) -> Self {
        Self {
            device_model: device_model.into(),
            android_version: android_version.into(),
            sdk,
            fingerprint: fingerprint.into(),
            kernel: kernel.into(),
            root_ready,
            hook_ready,
            freezer_ready,
            verified_targets: Vec::new(),
        }
    }

    pub fn with_verified_targets(mut self, verified_targets: Vec<VerifiedTarget>) -> Self {
        self.verified_targets = verified_targets;
        self
    }

    pub fn verified_target(&self) -> bool {
        self.verified_targets.iter().any(|target| {
            target.device_model.eq_ignore_ascii_case(&self.device_model) && target.sdk == self.sdk
        })
    }

    pub fn runtime_compatible(&self) -> bool {
        self.sdk >= 31 && !self.kernel.trim().is_empty()
    }

    pub fn compatibility_json(&self, capabilities: &[ControlCapability]) -> String {
        let capabilities_json = capabilities
            .iter()
            .map(|capability| {
                format!(
                    "{{\"name\":\"{}\",\"status\":\"{}\",\"evidence\":\"{}\"}}",
                    capability_name(capability.name),
                    capability_status(capability.status),
                    escape_json(&capability.evidence)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let limitations = self.limitations(capabilities);
        let limitations_json = limitations
            .iter()
            .map(|limitation| format!("\"{}\"", escape_json(limitation)))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"schema\":2,\"deviceModel\":\"{}\",\"androidVersion\":\"{}\",\"sdk\":{},\"fingerprint\":\"{}\",\"kernel\":\"{}\",\"rootReady\":{},\"hookReady\":{},\"freezerReady\":{},\"verifiedTarget\":{},\"runtimeCompatible\":{},\"controlAllowed\":{},\"limitations\":[{}],\"capabilities\":[{}]}}",
            escape_json(&self.device_model),
            escape_json(&self.android_version),
            self.sdk,
            escape_json(&self.fingerprint),
            escape_json(&self.kernel),
            self.root_ready,
            self.hook_ready,
            self.freezer_ready,
            self.verified_target(),
            self.runtime_compatible(),
            self.allows_control(capabilities),
            limitations_json,
            capabilities_json
        )
    }

    pub fn allows_control(&self, capabilities: &[ControlCapability]) -> bool {
        self.verified_target()
            && self.runtime_compatible()
            && self.root_ready
            && self.hook_ready
            && self.freezer_ready
            && capabilities.iter().all(|capability| {
                capability.name != CapabilityName::CgroupV2Freezer
                    || (capability.status == CapabilityStatus::Available
                        && capability.risk_level != RiskLevel::Disabled)
            })
    }

    fn limitations(&self, capabilities: &[ControlCapability]) -> Vec<String> {
        let mut limitations = Vec::new();
        if !self.verified_target() {
            limitations
                .push("runtime is not present in the module verified target list".to_owned());
        }
        if !self.runtime_compatible() {
            limitations.push("runtime requires Android API 31+ and a detected kernel".to_owned());
        }
        if !self.root_ready {
            limitations.push("root capability unavailable".to_owned());
        }
        if !self.hook_ready {
            limitations.push("hook bridge unavailable".to_owned());
        }
        if !self.freezer_ready {
            limitations.push("freezer capability unavailable".to_owned());
        }
        limitations.extend(
            capabilities
                .iter()
                .filter(|capability| capability.status != CapabilityStatus::Available)
                .map(|capability| capability.evidence.clone()),
        );
        limitations
    }
}

pub fn load_verified_targets(
    baseline_path: impl AsRef<Path>,
    allowlist_path: impl AsRef<Path>,
) -> Result<Vec<VerifiedTarget>, DaemonError> {
    let mut targets = Vec::new();
    if baseline_path.as_ref().exists() {
        let text = fs::read_to_string(baseline_path)?;
        let model = property(
            &text,
            &["rom.product", "ro.product.model", "ro.product.device"],
        );
        let sdk = property(&text, &["rom.sdk", "ro.build.version.sdk"])
            .and_then(|value| value.parse().ok())
            .or_else(|| property(&text, &["rom.android.version"]).and_then(android_version_sdk));
        if let (Some(model), Some(sdk)) = (model, sdk) {
            targets.push(VerifiedTarget::new(model, sdk));
        }
    }
    if allowlist_path.as_ref().exists() {
        for line in fs::read_to_string(allowlist_path)?.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let normalized = line.replace(',', " ");
            let mut fields = normalized.split_whitespace();
            let Some(model) = fields.next() else { continue };
            let sdk = fields.find_map(|field| {
                field
                    .strip_prefix("sdk=")
                    .unwrap_or(field)
                    .parse::<u32>()
                    .ok()
            });
            if let Some(sdk) = sdk {
                targets.push(VerifiedTarget::new(model, sdk));
            }
        }
    }
    targets.sort();
    targets.dedup();
    Ok(targets)
}

fn property<'a>(text: &'a str, keys: &[&str]) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        keys.contains(&key.trim()).then(|| value.trim())
    })
}

fn android_version_sdk(version: &str) -> Option<u32> {
    match version.trim() {
        "12" => Some(31),
        "12.1" => Some(32),
        "13" => Some(33),
        "14" => Some(34),
        "15" => Some(35),
        "16" => Some(36),
        _ => None,
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

fn capability_status(status: CapabilityStatus) -> &'static str {
    match status {
        CapabilityStatus::Available => "available",
        CapabilityStatus::Missing => "missing",
        CapabilityStatus::Degraded => "degraded",
        CapabilityStatus::Untested => "untested",
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
            other => vec![other],
        })
        .collect()
}
