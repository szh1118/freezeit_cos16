use crate::{
    domain::{
        capability::{CapabilityName, CapabilityStatus, ControlCapability, RiskLevel},
        operation::{ControlAction, ControlOperation, OperationResult},
        policy::{FallbackAction, FreezePolicy, ManagedApp},
        runtime::{ControlState, ProcessState, RuntimeProcess},
    },
    sys::{binder, cgroup},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionAction {
    Freeze,
    Postpone,
    AlternateFreezer,
    Signal,
    Terminate,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreezeDecision {
    pub action: DecisionAction,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendEnvironment {
    pub cgroup_available: bool,
    pub binder_available: bool,
    pub network_available: bool,
    pub wakelock_available: bool,
    pub screen_state_available: bool,
    pub hook_fresh: bool,
}

impl Default for BackendEnvironment {
    fn default() -> Self {
        Self {
            cgroup_available: true,
            binder_available: true,
            network_available: true,
            wakelock_available: true,
            screen_state_available: true,
            hook_fresh: true,
        }
    }
}

impl BackendEnvironment {
    pub fn detect() -> Self {
        Self {
            cgroup_available: std::path::Path::new("/sys/fs/cgroup").exists(),
            binder_available: binder::detect_binder_freezer_capability().status
                == CapabilityStatus::Available,
            network_available: false,
            wakelock_available: false,
            screen_state_available: false,
            hook_fresh: false,
        }
    }
}

pub struct SystemAwareCgroupBinderBackend {
    environment: BackendEnvironment,
}

impl SystemAwareCgroupBinderBackend {
    pub fn new(environment: BackendEnvironment) -> Self {
        Self { environment }
    }

    pub fn discover_capabilities(&self) -> Vec<ControlCapability> {
        let cgroup_capability = cgroup::detect_cgroup_v2_freezer_capability("/sys/fs/cgroup")
            .map(|capability| {
                let available = matches!(capability.status, CapabilityStatus::Available);
                capability_with_status(
                    CapabilityName::CgroupV2Freezer,
                    capability.status,
                    capability.evidence,
                    if available {
                        RiskLevel::Normal
                    } else {
                        RiskLevel::Disabled
                    },
                )
            })
            .unwrap_or_else(|error| {
                capability_with_status(
                    CapabilityName::CgroupV2Freezer,
                    CapabilityStatus::Missing,
                    format!("cgroup capability probe failed: {error}"),
                    RiskLevel::Disabled,
                )
            });
        let binder_capability = binder::detect_binder_freezer_capability();
        let binder_risk = match binder_capability.status {
            CapabilityStatus::Available => RiskLevel::Normal,
            CapabilityStatus::Untested | CapabilityStatus::Degraded => RiskLevel::Caution,
            CapabilityStatus::Missing => RiskLevel::Disabled,
        };
        vec![
            cgroup_capability,
            capability_with_status(
                CapabilityName::BinderFreezer,
                binder_capability.status,
                binder_capability.evidence,
                binder_risk,
            ),
            capability(
                CapabilityName::SignalControl,
                true,
                "kill(2) SIGSTOP/SIGCONT fallback backend",
            ),
            capability(
                CapabilityName::NetworkBreak,
                self.environment.network_available,
                "network break",
            ),
            capability(
                CapabilityName::WakelockControl,
                self.environment.wakelock_available,
                "wake-lock control",
            ),
        ]
    }

    pub fn can_freeze(
        &self,
        app: &ManagedApp,
        policy: &FreezePolicy,
        processes: &[RuntimeProcess],
    ) -> FreezeDecision {
        if !policy.is_control_allowed_for(app) {
            return FreezeDecision {
                action: DecisionAction::Skip,
                reason: "policy or protected package blocks control".to_owned(),
            };
        }

        if processes.is_empty() {
            return FreezeDecision {
                action: DecisionAction::Skip,
                reason: "no runtime processes".to_owned(),
            };
        }

        if processes.iter().any(|process| {
            matches!(
                process.proc_state,
                ProcessState::Foreground | ProcessState::Visible | ProcessState::Unknown
            )
        }) {
            return FreezeDecision {
                action: DecisionAction::Postpone,
                reason: "foreground, visible, or unknown process state".to_owned(),
            };
        }

        if !self.environment.hook_fresh || !self.environment.screen_state_available {
            return FreezeDecision {
                action: DecisionAction::Postpone,
                reason: "fresh hook and screen-state evidence required".to_owned(),
            };
        }

        if let Some(reason) = idle_blocker(processes) {
            return FreezeDecision {
                action: DecisionAction::Postpone,
                reason,
            };
        }

        if self.environment.cgroup_available && self.environment.binder_available {
            return FreezeDecision {
                action: DecisionAction::Freeze,
                reason: "cgroup and binder freezer available".to_owned(),
            };
        }

        // cgroup 可用但 binder 不可用时不能走 Freeze 路径：freeze_process 会把
        // binder::set_binder_freeze 当作前置条件（用 `?` 提前返回），必然失败并回滚，
        // 使本可用的 cgroup.freeze 精确冻结退化为信号回退。交给 fallback_decision
        // 按 policy 的 fallback_strategy 决定用信号还是推迟。
        if self.environment.cgroup_available && !self.environment.binder_available {
            return fallback_decision(
                policy,
                "cgroup freezer available but binder freezer unavailable; freeze_process requires binder",
            );
        }

        fallback_decision(policy, "preferred freezer unavailable")
    }

    pub fn fallback_after_freeze_apply_error(
        &self,
        policy: &FreezePolicy,
        error: &crate::app::error::DaemonError,
    ) -> FreezeDecision {
        let reason = if is_permission_denied(error) {
            format!("permission denied while applying cgroup.freeze: {error}")
        } else {
            format!("freezer apply failed: {error}")
        };
        fallback_decision(policy, &reason)
    }

    pub fn freeze_operation(
        &self,
        app: &ManagedApp,
        processes: &[RuntimeProcess],
        reason: impl Into<String>,
    ) -> ControlOperation {
        operation(
            app,
            processes,
            ControlAction::Freeze,
            OperationResult::Success,
            reason,
        )
    }

    pub fn unfreeze_operation(
        &self,
        app: &ManagedApp,
        processes: &[RuntimeProcess],
        reason: impl Into<String>,
    ) -> ControlOperation {
        operation(
            app,
            processes,
            ControlAction::Unfreeze,
            OperationResult::Success,
            reason,
        )
    }
}

pub fn fallback_decision(policy: &FreezePolicy, reason: &str) -> FreezeDecision {
    let fallback = match policy {
        FreezePolicy::Selected {
            fallback_strategy, ..
        } => fallback_strategy
            .first()
            .copied()
            .unwrap_or(FallbackAction::Skip),
    };

    let action = match fallback {
        FallbackAction::Postpone => DecisionAction::Postpone,
        FallbackAction::AlternateFreezer => DecisionAction::AlternateFreezer,
        FallbackAction::Signal => DecisionAction::Signal,
        FallbackAction::Terminate => DecisionAction::Terminate,
        FallbackAction::Skip => DecisionAction::Skip,
    };

    FreezeDecision {
        action,
        reason: reason.to_owned(),
    }
}

pub fn apply_cgroup_freeze(
    freeze_file: impl AsRef<std::path::Path>,
    frozen: bool,
) -> Result<(), crate::app::error::DaemonError> {
    cgroup::write_freeze_state(
        freeze_file,
        if frozen {
            cgroup::FreezeState::Frozen
        } else {
            cgroup::FreezeState::Thawed
        },
    )
}

fn capability(
    name: CapabilityName,
    available: bool,
    evidence: impl Into<String>,
) -> ControlCapability {
    ControlCapability {
        name,
        status: if available {
            CapabilityStatus::Available
        } else {
            CapabilityStatus::Missing
        },
        evidence: evidence.into(),
        checked_at_ms: 0,
        risk_level: if available {
            RiskLevel::Normal
        } else {
            RiskLevel::Disabled
        },
    }
}

fn capability_with_status(
    name: CapabilityName,
    status: CapabilityStatus,
    evidence: impl Into<String>,
    risk_level: RiskLevel,
) -> ControlCapability {
    ControlCapability {
        name,
        status,
        evidence: evidence.into(),
        checked_at_ms: 0,
        risk_level,
    }
}

fn idle_blocker(processes: &[RuntimeProcess]) -> Option<String> {
    let busy = processes.iter().find(|process| {
        process.control_state == ControlState::PendingFreeze
            && process.binder_state.as_deref().is_some_and(|state| {
                let normalized = state.to_ascii_lowercase();
                normalized.contains("busy")
                    || normalized.contains("sync_txn")
                    || normalized.contains("txns_pending")
                    || normalized.contains("binder_pending")
            })
    })?;

    Some(format!(
        "pending freeze idle evidence not satisfied for pid {}: {}",
        busy.pid,
        busy.binder_state.as_deref().unwrap_or("unknown")
    ))
}

fn is_permission_denied(error: &crate::app::error::DaemonError) -> bool {
    matches!(
        error,
        crate::app::error::DaemonError::Io(io_error)
            if io_error.kind() == std::io::ErrorKind::PermissionDenied
    )
}

fn operation(
    app: &ManagedApp,
    processes: &[RuntimeProcess],
    action: ControlAction,
    result: OperationResult,
    reason: impl Into<String>,
) -> ControlOperation {
    ControlOperation {
        operation_id: 0,
        timestamp_ms: 0,
        package_name: app.package_name.clone(),
        uid: app.uid,
        pid_list: processes.iter().map(|process| process.pid).collect(),
        action,
        backend: "SystemAwareCgroupBinderBackend".to_owned(),
        reason: reason.into(),
        result,
        details: format!("process_count={}", processes.len()),
    }
}

pub fn mark_processes_frozen(processes: &mut [RuntimeProcess]) {
    for process in processes {
        process.control_state = ControlState::Frozen;
    }
}

pub fn mark_processes_running(processes: &mut [RuntimeProcess]) {
    for process in processes {
        process.control_state = ControlState::Running;
    }
}
