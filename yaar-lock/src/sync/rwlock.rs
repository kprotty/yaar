use crate::ThreadEvent;
use core::{
    marker::PhantomData,
    sync::atomic::{Ordering, AtomicUsize},
};

#[cfg(feature = "os")]
pub use self::if_os::*;
#[cfg(feature = "os")]
mod if_os {
    use super::*;
    use crate::OsThreadEvent;

    #[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
    pub type RwLock<T> = RawRwLock<T, OsThreadEvent>;

    #[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
    pub type RwLockReadGuard<'a, T> = RawRwLockReadGuard<'a, T, OsThreadEvent>;

    #[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
    pub type RwLockWriteGuard<'a, T> = RawRwLockWriteGuard<'a, T, OsThreadEvent>;
}

pub type RawRwLock<T, E> = lock_api::RwLock<CoreRwLock<E>, T>;
pub type RawRwLockReadGuard<'a, T, E> = lock_api::RwLockReadGuard<'a, CoreRwLock<E>, T>;
pub type RawRwLockWriteGuard<'a, T, E> = lock_api::RwLockWriteGuard<'a, CoreRwLock<E>, T>;


const PARKED: usize = 0b001;
const WRITE: usize = 0b010;
const READ: usize = 0b100;

pub struct CoreRwLock<E: ThreadEvent> {
    state: AtomicUsize,
    queue: AtomicUsize,
    phantom: PhantomData<E>,
}

unsafe impl<E: ThreadEvent> Send for CoreRwLock<E> {}
unsafe impl<E: ThreadEvent> Sync for CoreRwLock<E> {}

unsafe impl<E: ThreadEvent> lock_api::RawRwLock for CoreRwLock<E> {
    const INIT: Self = Self {
        state: AtomicUsize::new(0),
        queue: AtomicUsize::new(0),
        phantom: PhantomData,
    };

    type GuardMarker = lock_api::GuardSend;

    #[inline]
    fn try_lock_exclusive(&self) -> bool {
        self.state
            .compare_exchange_weak(0, WRITE, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    #[inline]
    fn lock_exclusive(&self) {
        if !self.try_lock_exclusive() {
            self.lock_exclusive_slow();
        }
    }

    #[inline]
    fn unlock_exclusive(&self) {
        if self
            .state
            .compare_exchange(WRITE, 0, Ordering::Release, Ordering::Relaxed)
            .is_err()
        {
            self.unlock_exclusive_slow();
        }
    }

    #[inline]
    fn try_lock_shared(&self) -> bool {
        let state = self.state.load(Ordering::Relaxed);
        if state & (WRITE | PARKED) == 0 {
            if let Some(new_state) = state.checked_add(READ) {
                return match self.state.compare_exchange_weak(
                    state,
                    new_state,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => true,
                    Err(_) => self.try_lock_shared_slow(),
                };
            }
        }
        false
    }

    #[inline]
    fn lock_shared(&self) {
        if !self.try_lock_shared() {
            self.lock_shared_slow();
        }
    }

    #[inline]
    fn unlock_shared(&self) {
        let state = self.state.fetch_sub(READ, Ordering::Release);
        if state == (READ | PARKED) {
            self.unlock_shared_slow();
        }
    }
}

impl<E: ThreadEvent> CoreRwLock<E> {
    #[cold]
    fn lock_exclusive_slow(&self) {
        let node = WaitNode::new(true);
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state & WRITE == 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state | WRITE,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(e) => state = e,
                }
                continue;
            }

            if state & PARKED == 0 {
                if let Err(e) = self.state.compare_exchange_weak(
                    state,
                    state | PARKED,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    state = e;
                    continue;
                }
            }

            unimplemented!();
        }
    }

    #[cold]
    fn unlock_exclusive_slow(&self) {
        self.state.fetch_sub(WRITE, Ordering::Release);
        unimplemented!();
    }

    #[cold]
    fn try_lock_shared_slow(&self) -> bool {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state & (WRITE | PARKED) == 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state.checked_add(READ)
                        .expect("RwLock read overflow"),
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return true,
                    Err(e) => state = e,
                }
                continue;
            }
            return false;
        }
    }

    #[cold]
    fn lock_shared_slow(&self) {
        unimplemented!();
    }

    #[cold]
    fn unlock_shared_slow(&self) {
        unimplemented!();
    }
}
