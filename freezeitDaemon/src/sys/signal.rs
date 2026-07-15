pub const SIGSTOP_NUMBER: i32 = 19;
pub const SIGCONT_NUMBER: i32 = 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalAction {
    Stop,
    Continue,
}

impl SignalAction {
    pub fn signal_number(self) -> i32 {
        match self {
            Self::Stop => SIGSTOP_NUMBER,
            Self::Continue => SIGCONT_NUMBER,
        }
    }
}

pub fn is_signal_allowed(test_mode: bool, pid: i32) -> bool {
    test_mode && pid > 0
}

pub fn send_signal(pid: i32, action: SignalAction) -> Result<(), crate::app::error::DaemonError> {
    if pid <= 0 {
        return Err(crate::app::error::DaemonError::system(
            "refusing to signal non-positive pid",
        ));
    }

    match action {
        SignalAction::Stop => {
            send_raw_signal(pid, SignalAction::Stop)?;
            if let Err(ledger_error) = crate::sys::procfs::record_freezeit_signal_stop(pid) {
                let rollback_error = send_raw_signal(pid, SignalAction::Continue).err();
                return Err(crate::app::error::DaemonError::system(match rollback_error {
                    Some(rollback_error) => format!(
                        "SIGSTOP succeeded but ownership ledger failed: {ledger_error}; SIGCONT rollback failed: {rollback_error}"
                    ),
                    None => format!(
                        "SIGSTOP succeeded but ownership ledger failed: {ledger_error}; SIGCONT rollback succeeded"
                    ),
                }));
            }
            Ok(())
        }
        SignalAction::Continue => {
            let was_owned_stop = crate::sys::procfs::take_freezeit_signal_stop(pid)?;
            if let Err(signal_error) = send_raw_signal(pid, SignalAction::Continue) {
                if was_owned_stop {
                    if let Err(restore_error) = crate::sys::procfs::record_freezeit_signal_stop(pid)
                    {
                        return Err(crate::app::error::DaemonError::system(format!(
                            "SIGCONT failed: {signal_error}; failed to restore ownership ledger: {restore_error}"
                        )));
                    }
                }
                return Err(signal_error);
            }
            Ok(())
        }
    }
}

fn send_raw_signal(pid: i32, action: SignalAction) -> Result<(), crate::app::error::DaemonError> {
    let result = unsafe { libc::kill(pid, action.signal_number()) };
    if result == 0 {
        Ok(())
    } else {
        Err(crate::app::error::DaemonError::from(
            std::io::Error::last_os_error(),
        ))
    }
}
