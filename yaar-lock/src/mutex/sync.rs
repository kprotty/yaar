use crate::ThreadParker;
use core::task::Poll;

#[cfg(feature = "os")]
pub type Mutex<T> = RawMutex<T, crate::OsThreadParker>;

#[cfg(feature = "os")]
pub type MutexGuard<'a, T> = RawMutexGuard<'a, T, crate::OsThreadParker>;

pub type RawMutex<T, Parker> = lock_api::Mutex<SyncMutex<Parker>, T>;

pub type RawMutexGuard<'a, T, Parker> = lock_api::MutexGuard<'a, SyncMutex<Parker>, T>;

pub struct SyncMutex<Parker>(WordLock<Parker>);

impl<Parker> SyncMutex<Parker> {
    pub const fn new() -> Self {
        Self(WordLock::new())
    }
}

unsafe impl<Parker: ThreadParker<Context = ()>> lock_api::RawMutex for SyncMutex<Parker> {
    const INIT: Self = Self::new();

    type GuardMarker = lock_api::GuardSend;

    fn try_lock(&self) -> bool {
        self.0.try_lock()
    }

    fn lock(&self) {
        if !self.try_lock() {
            let result = self.0.lock_slow(());
            debug_assert_eq!(result, Poll::Ready(()));
        }
    }

    fn unlock(&self) {
        self.0.unlock();
    }
}