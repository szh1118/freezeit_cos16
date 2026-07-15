use std::{collections::BTreeMap, sync::Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingFreeze {
    pub package_name: String,
    pub uid: u32,
    pub due_at_ms: u128,
}

#[derive(Debug, Default)]
pub struct FreezeScheduler {
    pending: Mutex<BTreeMap<(String, u32), PendingFreeze>>,
}

impl FreezeScheduler {
    pub fn schedule_background(
        &mut self,
        package_name: impl Into<String>,
        uid: u32,
        now_ms: u128,
        delay_ms: u64,
    ) {
        let package_name = package_name.into();
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                (package_name.clone(), uid),
                PendingFreeze {
                    package_name,
                    uid,
                    due_at_ms: now_ms + u128::from(delay_ms),
                },
            );
    }

    pub fn cancel_foreground(&mut self, package_name: &str, uid: u32) -> Option<PendingFreeze> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&(package_name.to_owned(), uid))
    }

    pub fn due_at(&self, now_ms: u128) -> Vec<PendingFreeze> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let due_keys = pending
            .iter()
            .filter(|(_, pending)| pending.due_at_ms <= now_ms)
            .map(|(identity, _)| identity.clone())
            .collect::<Vec<_>>();
        due_keys
            .into_iter()
            .filter_map(|identity| pending.remove(&identity))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_jobs_are_claimed_once() {
        let mut scheduler = FreezeScheduler::default();
        scheduler.schedule_background("com.example.app", 10_123, 100, 5_000);

        assert_eq!(scheduler.due_at(5_100).len(), 1);
        assert!(scheduler.due_at(5_101).is_empty());
    }
}
