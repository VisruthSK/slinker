use std::sync::{Mutex, MutexGuard};

pub(super) struct Guarded<T>(Mutex<T>);

impl<T> Guarded<T> {
    pub(super) fn new(value: T) -> Self {
        Self(Mutex::new(value))
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, T> {
        self.0
            .lock()
            .expect("analysis state lock is never poisoned")
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
