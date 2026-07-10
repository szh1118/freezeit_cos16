#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeProcess {
    pub pid: i32,
    pub uid: u32,
    pub package_name: String,
    pub process_name: String,
    pub proc_state: ProcessState,
    pub control_state: ControlState,
    pub cgroup_freeze_path: Option<String>,
    pub binder_state: Option<String>,
    pub start_time_ticks: Option<u64>,
    pub last_seen_at_ms: u128,
}

impl RuntimeProcess {
    pub fn identity_matches(&self, package_name: &str, uid: u32) -> bool {
        self.package_name == package_name && self.uid == uid
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Foreground,
    Visible,
    Service,
    Cached,
    Empty,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlState {
    Running,
    PendingFreeze,
    Frozen,
    Unfreezing,
    Unknown,
}
