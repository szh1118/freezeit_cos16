use std::collections::BTreeMap;

use crate::app::{command_runner::run_command, error::DaemonError};

pub const DOWNLOAD_THRESHOLD_BYTES_PER_SEC: u64 = 5 * 1024 * 1024;
pub const DOWNLOAD_RETRY_DELAY_MS: u64 = 30_000;
pub const INITIAL_SAMPLE_DELAY_MS: u64 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadDeferralAction {
    Proceed,
    Postpone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UidRxSample {
    Value(u64),
    Missing,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sample {
    rx_bytes: u64,
    sampled_at_ms: u128,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DownloadDeferral {
    samples: BTreeMap<u32, Sample>,
}

impl DownloadDeferral {
    pub fn clear_uid(&mut self, uid: u32) {
        self.samples.remove(&uid);
    }

    pub fn evaluate(
        &mut self,
        uid: u32,
        package_name: &str,
        sample: UidRxSample,
        sampled_at_ms: u128,
    ) -> DownloadDeferralAction {
        if !is_candidate_package(package_name) {
            self.samples.remove(&uid);
            return DownloadDeferralAction::Proceed;
        }
        let UidRxSample::Value(rx_bytes) = sample else {
            // A gap makes the elapsed interval unknown. Keeping the old baseline would dilute a
            // later burst across the unobserved period and could incorrectly permit freezing.
            self.samples.remove(&uid);
            return DownloadDeferralAction::Postpone;
        };
        let Some(previous) = self.samples.insert(
            uid,
            Sample {
                rx_bytes,
                sampled_at_ms,
            },
        ) else {
            return DownloadDeferralAction::Postpone;
        };
        if sampled_at_ms <= previous.sampled_at_ms || rx_bytes < previous.rx_bytes {
            return DownloadDeferralAction::Postpone;
        }
        let elapsed_ms = sampled_at_ms - previous.sampled_at_ms;
        let bytes_per_second = u128::from(rx_bytes - previous.rx_bytes) * 1_000 / elapsed_ms;
        if bytes_per_second > u128::from(DOWNLOAD_THRESHOLD_BYTES_PER_SEC) {
            DownloadDeferralAction::Postpone
        } else {
            DownloadDeferralAction::Proceed
        }
    }
}

pub fn is_candidate_package(package_name: &str) -> bool {
    const DOWNLOAD_CANDIDATE_PACKAGES: &[&str] = &[
        "com.baidu.netdisk",
        "com.quark.clouddrive",
        "com.google.android.apps.docs",
        "com.pikpak.android",
        "com.trim.app",
    ];

    DOWNLOAD_CANDIDATE_PACKAGES
        .iter()
        .any(|candidate| package_name.eq_ignore_ascii_case(candidate))
}

pub fn parse_uid_rx_bytes(netstats: &str) -> Result<BTreeMap<u32, u64>, DaemonError> {
    let mut values = BTreeMap::new();
    let mut in_uid_table = false;
    for line in netstats.lines().map(str::trim) {
        if line == "mAppUidStatsMap:" {
            in_uid_table = true;
            continue;
        }
        if in_uid_table && line == "mStatsMapA:" {
            break;
        }
        if !in_uid_table {
            continue;
        }
        let mut fields = line.split_whitespace();
        let (Some(uid), Some(rx_bytes)) = (fields.next(), fields.next()) else {
            continue;
        };
        if let (Ok(uid), Ok(rx_bytes)) = (uid.parse::<u32>(), rx_bytes.parse::<u64>()) {
            values.insert(uid, rx_bytes);
        }
    }
    if values.is_empty() {
        Err(DaemonError::system(
            "Android netstats UID rx table unavailable",
        ))
    } else {
        Ok(values)
    }
}

pub fn sample_uid_rx_bytes(uid: u32) -> Result<Option<u64>, DaemonError> {
    Ok(sample_uid_rx_bytes_map()?.get(&uid).copied())
}

pub fn sample_uid_rx_bytes_map() -> Result<BTreeMap<u32, u64>, DaemonError> {
    let output = run_command("/system/bin/dumpsys", &["netstats"])?;
    if !output.status.success() {
        return Err(DaemonError::system(format!(
            "dumpsys netstats failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    parse_uid_rx_bytes(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_sample_resets_the_old_rate_baseline() {
        let mut deferral = DownloadDeferral::default();
        let uid = 10_123;
        let package = "com.baidu.netdisk";

        assert_eq!(
            deferral.evaluate(uid, package, UidRxSample::Value(0), 0),
            DownloadDeferralAction::Postpone
        );
        assert_eq!(
            deferral.evaluate(uid, package, UidRxSample::Failed, 1_000),
            DownloadDeferralAction::Postpone
        );
        assert_eq!(
            deferral.evaluate(uid, package, UidRxSample::Value(8 * 1024 * 1024), 31_000,),
            DownloadDeferralAction::Postpone
        );
    }

    #[test]
    fn candidate_matching_requires_an_explicit_package_identity() {
        assert!(is_candidate_package("com.baidu.netdisk"));
        assert!(is_candidate_package("com.pikpak.android"));
        assert!(!is_candidate_package("com.example.pikpakhelper"));
    }
}
