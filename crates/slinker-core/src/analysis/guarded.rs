use crate::profile::{self, Probe};
use std::sync::{Mutex, MutexGuard, TryLockError};

pub(super) struct Guarded<T>(Mutex<T>);

impl<T> Guarded<T> {
    pub(super) fn new(value: T) -> Self {
        Self(Mutex::new(value))
    }

    #[track_caller]
    pub(super) fn lock(&self) -> MutexGuard<'_, T> {
        contended(&self.0)
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

#[track_caller]
pub(super) fn contended<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::WouldBlock) => {
            let site = std::panic::Location::caller();
            let _waiting = profile::span(Probe::LockWait);
            let started = std::time::Instant::now();
            let guard = mutex.lock().expect("analysis state lock is never poisoned");
            profile::lock_wait(site, started.elapsed());
            guard
        }
        Err(TryLockError::Poisoned(_)) => panic!("analysis state lock is never poisoned"),
    }
}
