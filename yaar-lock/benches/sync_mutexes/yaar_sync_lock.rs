use lock_api::RawMutex;
use std::cell::UnsafeCell;
use yaar_lock::{core::Lock, event::OsAutoResetEvent};

pub struct Mutex<T> {
    lock: Lock<OsAutoResetEvent>,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    pub fn new(value: T) -> Self {
        Self {
            lock: Lock::new(),
            value: UnsafeCell::new(value),
        }
    }

    pub fn lock<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        self.lock.lock();
        let result = f(unsafe { &mut *self.value.get() });
        self.lock.unlock();
        result
    }
}
