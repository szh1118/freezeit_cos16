#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedApp {
    pub package_name: String,
    pub user_id: u32,
    pub uid: u32,
    pub label: String,
    pub is_system_app: bool,
    pub protected_reason: Option<ProtectedReason>,
    pub policy_id: String,
    pub last_seen_baseline: String,
}

impl ManagedApp {
    pub fn policy_identity(&self) -> (&str, u32) {
        (&self.package_name, self.user_id)
    }

    pub fn is_protected(&self) -> bool {
        self.protected_reason.is_some()
    }

    pub fn apply_protected_defaults(&mut self) {
        if self.protected_reason.is_some() || self.is_system_app {
            self.protected_reason = self
                .protected_reason
                .or(Some(ProtectedReason::SystemCritical));
            self.policy_id = format!("protected:{}", self.package_name);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectedReason {
    Manager,
    Launcher,
    InputMethod,
    RootManager,
    HookManager,
    SystemCritical,
    UserProtected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreezeMode {
    Protected,
    Free,
    Freeze,
    FreezeWithRestrictions,
    SignalStop,
    Terminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlMethod {
    CgroupBinderFreeze,
    SignalStop,
    Terminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RestrictionRequests {
    pub network_break: bool,
    pub wakelock_restriction: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForegroundStrategy {
    Strict,
    Permissive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackAction {
    Postpone,
    AlternateFreezer,
    Signal,
    Terminate,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreezePolicy {
    Selected {
        mode: FreezeMode,
        delay_ms: u64,
        foreground_strategy: ForegroundStrategy,
        allow_network_restriction: bool,
        allow_wakelock_restriction: bool,
        fallback_strategy: Vec<FallbackAction>,
        updated_at_ms: u128,
    },
}

impl FreezePolicy {
    pub fn from_legacy_mode(mode: i32, permissive: bool) -> Self {
        let (mode, allow_network_restriction, fallback_strategy) = match mode {
            10 => (
                FreezeMode::Terminate,
                false,
                vec![FallbackAction::Postpone, FallbackAction::Skip],
            ),
            20 => (
                FreezeMode::SignalStop,
                false,
                vec![FallbackAction::Postpone, FallbackAction::Skip],
            ),
            21 => (
                FreezeMode::SignalStop,
                true,
                vec![FallbackAction::Postpone, FallbackAction::Skip],
            ),
            30 => (
                FreezeMode::Freeze,
                false,
                vec![FallbackAction::Signal, FallbackAction::Skip],
            ),
            31 => (
                FreezeMode::Freeze,
                true,
                vec![FallbackAction::Signal, FallbackAction::Skip],
            ),
            40 | 50 => (FreezeMode::Protected, false, vec![FallbackAction::Skip]),
            _ => (FreezeMode::Free, false, vec![FallbackAction::Skip]),
        };

        Self::Selected {
            mode,
            delay_ms: 0,
            foreground_strategy: if permissive {
                ForegroundStrategy::Permissive
            } else {
                ForegroundStrategy::Strict
            },
            allow_network_restriction,
            allow_wakelock_restriction: false,
            fallback_strategy,
            updated_at_ms: 0,
        }
    }

    pub fn protected_default() -> Self {
        Self::Selected {
            mode: FreezeMode::Protected,
            delay_ms: 0,
            foreground_strategy: ForegroundStrategy::Strict,
            allow_network_restriction: false,
            allow_wakelock_restriction: false,
            fallback_strategy: vec![FallbackAction::Skip],
            updated_at_ms: 0,
        }
    }

    pub fn is_control_allowed_for(&self, app: &ManagedApp) -> bool {
        if app.is_system_app || app.is_protected() {
            return false;
        }

        self.control_method().is_some()
    }

    pub fn control_method(&self) -> Option<ControlMethod> {
        match self {
            Self::Selected {
                mode: FreezeMode::Freeze | FreezeMode::FreezeWithRestrictions,
                ..
            } => Some(ControlMethod::CgroupBinderFreeze),
            Self::Selected {
                mode: FreezeMode::SignalStop,
                ..
            } => Some(ControlMethod::SignalStop),
            Self::Selected {
                mode: FreezeMode::Terminate,
                ..
            } => Some(ControlMethod::Terminate),
            Self::Selected {
                mode: FreezeMode::Protected | FreezeMode::Free,
                ..
            } => None,
        }
    }

    pub fn requested_restrictions(&self) -> RestrictionRequests {
        match self {
            Self::Selected {
                allow_network_restriction,
                allow_wakelock_restriction,
                ..
            } => RestrictionRequests {
                network_break: *allow_network_restriction,
                wakelock_restriction: *allow_wakelock_restriction,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ControlMethod, FallbackAction, ForegroundStrategy, FreezeMode, FreezePolicy, ManagedApp,
        RestrictionRequests,
    };

    fn freeze_policy() -> FreezePolicy {
        FreezePolicy::Selected {
            mode: FreezeMode::Freeze,
            delay_ms: 0,
            foreground_strategy: ForegroundStrategy::Strict,
            allow_network_restriction: false,
            allow_wakelock_restriction: false,
            fallback_strategy: vec![FallbackAction::Skip],
            updated_at_ms: 0,
        }
    }

    fn system_app_without_reason() -> ManagedApp {
        ManagedApp {
            package_name: "com.android.unclassified".to_owned(),
            user_id: 0,
            uid: 1_234,
            label: "Unclassified system app".to_owned(),
            is_system_app: true,
            protected_reason: None,
            policy_id: "test".to_owned(),
            last_seen_baseline: "test".to_owned(),
        }
    }

    #[test]
    fn system_apps_are_never_controlled_without_a_protected_reason() {
        assert!(!freeze_policy().is_control_allowed_for(&system_app_without_reason()));
    }

    #[test]
    fn legacy_modes_preserve_their_requested_control_method_and_restrictions() {
        assert_eq!(
            FreezePolicy::from_legacy_mode(10, false).control_method(),
            Some(ControlMethod::Terminate)
        );
        assert_eq!(
            FreezePolicy::from_legacy_mode(20, false).control_method(),
            Some(ControlMethod::SignalStop)
        );
        assert_eq!(
            FreezePolicy::from_legacy_mode(31, false).control_method(),
            Some(ControlMethod::CgroupBinderFreeze)
        );
        assert_eq!(
            FreezePolicy::from_legacy_mode(21, false).requested_restrictions(),
            RestrictionRequests {
                network_break: true,
                wakelock_restriction: false,
            }
        );
    }
}
