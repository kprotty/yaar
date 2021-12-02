use super::parking_lot::{Mutex, MutexGuard};

pub type Locked<'a, T> = MutexGuard<'a, T>;

pub struct TryLock<T>(Mutex<T>);

impl<T: Default> Default for TryLock<T> {
    fn default() -> Self {
        Self::new(T::default())
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
