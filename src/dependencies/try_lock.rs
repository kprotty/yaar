pub use self::try_lock_impl::{Locked, TryLock};

#[cfg(feature = "try_lock")]
mod try_lock_impl {
    pub use try_lock::{Locked, TryLock};
}

#[cfg(not(feature = "try_lock"))]
mod try_lock_impl {
    use super::super::sync::{Mutex, MutexGuard};

    pub type Locked = MutexGuard;
    pub struct TryLock<T>(Mutex<T>);

    impl<T: Default> Default for TryLock<T> {
        fn default() -> Self {
            Self(Mutex::default())
        }
    }

    impl<T> TryLock<T> {
        pub fn new(value: T) -> Self {
            Self(Mutex::new(value))
        }

        pub fn try_lock(&self) -> Option<Locked<'_, T>> {
            self.0.try_lock()
        }
    }
}
