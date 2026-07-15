use crate::app::error::DaemonError;
use crate::domain::policy::FreezePolicy;

pub fn migrate_legacy_config() -> Result<(), DaemonError> {
    Ok(())
}

pub fn normalize_legacy_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyPolicyRecord {
    pub package_or_uid: String,
    pub mode: i32,
    pub permissive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyPolicyTarget {
    Uid(u32),
    PackageName(String),
}

impl LegacyPolicyRecord {
    pub fn target(&self) -> Option<LegacyPolicyTarget> {
        parse_legacy_policy_target(&self.package_or_uid)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyLabelRecord {
    pub package_name: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigratedLegacyConfig {
    pub policies: Vec<(LegacyPolicyRecord, FreezePolicy)>,
    pub labels: Vec<LegacyLabelRecord>,
    pub settings: Vec<u8>,
}

pub fn parse_legacy_policy_line(line: &str) -> Option<LegacyPolicyRecord> {
    let line = normalize_legacy_line(line)?;
    let mut parts = line.split_whitespace();
    Some(LegacyPolicyRecord {
        package_or_uid: parts.next()?.to_owned(),
        mode: parts.next()?.parse().ok()?,
        permissive: parts.next().unwrap_or("1") != "0",
    })
}

pub fn parse_legacy_policy_target(token: &str) -> Option<LegacyPolicyTarget> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    if let Ok(uid) = token.parse::<u32>() {
        return Some(LegacyPolicyTarget::Uid(uid));
    }

    let Some((legacy_uid, encoded_uid)) = token.split_once("uid") else {
        return Some(LegacyPolicyTarget::PackageName(token.to_owned()));
    };
    if !legacy_uid.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(LegacyPolicyTarget::PackageName(token.to_owned()));
    }

    let legacy_uid = legacy_uid.parse::<u32>().ok()?;
    let encoded_uid = encoded_uid.parse::<u32>().ok()?;
    (legacy_uid == encoded_uid).then_some(LegacyPolicyTarget::Uid(legacy_uid))
}

pub fn parse_legacy_label_line(line: &str) -> Option<LegacyLabelRecord> {
    let line = normalize_legacy_line(line)?;
    let (package_name, label) = line.split_once("####")?;
    let package_name = package_name.trim();
    let label = label.trim();
    if package_name.is_empty() || label.is_empty() {
        return None;
    }

    Some(LegacyLabelRecord {
        package_name: package_name.to_owned(),
        label: label.to_owned(),
    })
}

pub fn migrate_legacy_files(
    app_config: &str,
    app_label: &str,
    settings: &[u8],
) -> MigratedLegacyConfig {
    let policies = app_config
        .lines()
        .filter_map(parse_legacy_policy_line)
        .filter(|record| record.target().is_some())
        .map(|record| {
            let policy = migrate_legacy_policy(&record);
            (record, policy)
        })
        .collect();
    let labels = app_label
        .lines()
        .filter_map(parse_legacy_label_line)
        .collect();

    MigratedLegacyConfig {
        policies,
        labels,
        settings: settings.to_vec(),
    }
}

pub fn migrate_legacy_policy(record: &LegacyPolicyRecord) -> FreezePolicy {
    FreezePolicy::from_legacy_mode(record.mode, record.permissive)
}

#[cfg(test)]
mod tests {
    use super::{
        migrate_legacy_files, migrate_legacy_policy, parse_legacy_policy_line,
        parse_legacy_policy_target, LegacyPolicyTarget,
    };
    use crate::domain::policy::FreezePolicy;

    #[test]
    fn legacy_uid_parser_only_accepts_canonical_uid_tokens() {
        assert_eq!(
            parse_legacy_policy_target("10123uid10123"),
            Some(LegacyPolicyTarget::Uid(10_123))
        );
        assert_eq!(
            parse_legacy_policy_target("com.example.uid12345"),
            Some(LegacyPolicyTarget::PackageName(
                "com.example.uid12345".to_owned()
            ))
        );
        assert_eq!(parse_legacy_policy_target("10123uid99999"), None);
    }

    #[test]
    fn migration_uses_the_shared_legacy_mode_conversion() {
        let record = parse_legacy_policy_line("com.example.app 21 1").expect("legacy policy");

        assert_eq!(
            migrate_legacy_policy(&record),
            FreezePolicy::from_legacy_mode(21, true)
        );
    }

    #[test]
    fn migration_omits_invalid_canonical_uid_targets() {
        let migrated = migrate_legacy_files("10123uid99999 30 1\n", "", &[]);

        assert!(migrated.policies.is_empty());
    }
}
