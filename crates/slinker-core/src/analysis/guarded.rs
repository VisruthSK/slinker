use crate::profile::{self, Probe};
use std::sync::{Mutex, MutexGuard, TryLockError};

pub(super) struct Guarded<T>(Mutex<T>);

impl<T> Guarded<T> {
    pub(super) fn new(value: T) -> Self {
        Self(Mutex::new(value))
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, T> {
        match self.0.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => {
                let _waiting = profile::span(Probe::LockWait);
                self.0
                    .lock()
                    .expect("analysis state lock is never poisoned")
            }
            Err(TryLockError::Poisoned(_)) => panic!("analysis state lock is never poisoned"),
        }
    }

    pub(super) fn into_inner(self) -> T {
        self.0
            .into_inner()
            .expect("analysis state lock is never poisoned")
    }
}

impl<T: Default> Default for Guarded<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}
