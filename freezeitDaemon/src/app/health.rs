#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Active,
    Degraded,
    Inactive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleHealth {
    pub manager_ready: bool,
    pub daemon_ready: bool,
    pub hook_ready: bool,
    pub root_ready: bool,
    pub freezer_ready: bool,
    pub policy_ready: bool,
    pub status: HealthStatus,
    pub degraded_reasons: Vec<String>,
}

impl ModuleHealth {
    pub fn inactive(reason: impl Into<String>) -> Self {
        Self {
            manager_ready: false,
            daemon_ready: false,
            hook_ready: false,
            root_ready: false,
            freezer_ready: false,
            policy_ready: false,
            status: HealthStatus::Inactive,
            degraded_reasons: vec![reason.into()],
        }
    }

    pub fn evaluate(
        manager_ready: bool,
        daemon_ready: bool,
        hook_ready: bool,
        root_ready: bool,
        freezer_ready: bool,
        policy_ready: bool,
    ) -> Self {
        let mut degraded_reasons = Vec::new();
        if !daemon_ready {
            degraded_reasons.push("daemon not initialized".to_owned());
        }
        if !hook_ready {
            degraded_reasons.push("hook bridge unavailable".to_owned());
        }
        if !root_ready {
            degraded_reasons.push("root capability unavailable".to_owned());
        }
        if !freezer_ready {
            degraded_reasons.push("freezer capability unavailable".to_owned());
        }
        if !policy_ready {
            degraded_reasons.push("policy unavailable".to_owned());
        }

        let status = if manager_ready
            && daemon_ready
            && hook_ready
            && root_ready
            && freezer_ready
            && policy_ready
        {
            HealthStatus::Active
        } else if manager_ready || daemon_ready {
            HealthStatus::Degraded
        } else {
            HealthStatus::Inactive
        };

        Self {
            manager_ready,
            daemon_ready,
            hook_ready,
            root_ready,
            freezer_ready,
            policy_ready,
            status,
            degraded_reasons,
        }
    }

    pub fn is_safe_for_control(&self) -> bool {
        self.status == HealthStatus::Active
    }

    pub fn with_hook_bridge(
        manager_ready: bool,
        daemon_ready: bool,
        root_ready: bool,
        freezer_ready: bool,
        policy_ready: bool,
        hook_ready_for_control: bool,
        hook_reason: Option<String>,
    ) -> Self {
        let mut health = Self::evaluate(
            manager_ready,
            daemon_ready,
            hook_ready_for_control,
            root_ready,
            freezer_ready,
            policy_ready,
        );

        if let Some(reason) = hook_reason {
            if !health.degraded_reasons.iter().any(|item| item == &reason) {
                health.degraded_reasons.push(reason);
            }
        }

        health
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_capability_failures(
        manager_ready: bool,
        daemon_ready: bool,
        hook_ready: bool,
        root_ready: bool,
        package_inventory_ready: bool,
        freezer_ready: bool,
        network_ready: bool,
        wakelock_ready: bool,
        screen_state_ready: bool,
    ) -> Self {
        let mut health = Self::evaluate(
            manager_ready,
            daemon_ready,
            hook_ready,
            root_ready,
            freezer_ready,
            package_inventory_ready,
        );

        if !package_inventory_ready {
            health
                .degraded_reasons
                .push("package inventory unavailable".to_owned());
        }
        if !network_ready {
            health
                .degraded_reasons
                .push("network control unavailable".to_owned());
        }
        if !wakelock_ready {
            health
                .degraded_reasons
                .push("wake-lock control unavailable".to_owned());
        }
        if !screen_state_ready {
            health
                .degraded_reasons
                .push("screen-state detection unavailable".to_owned());
        }

        health
    }
}
