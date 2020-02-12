use super::WaitNode;
use crate::ThreadEvent;
use core::{
    marker::PhantomData,
    sync::atomic::{spin_loop_hint, Ordering, AtomicUsize},
};

const READ: usize = 0b100;
const WRITE: usize = 0b010;
const WAITING: usize = 0b001;

pub struct CoreRwLock<E: ThreadEvent> {
    state: AtomicUsize,
    queue: AtomicUsize,
    phantom: PhantomData<E>,
}

unsafe impl<E: ThreadEvent> lock_api::RawRwLock for CoreRwLock<E> {
    const INIT: Self = Self::new();

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
        if self.state.compare_exchange_weak(WRITE, 0, Ordering::Release, Ordering::Relaxed).is_err() {
            self.unlock_exclusive_slow();
        }
    }

    #[inline]
    fn try_lock_shared(&self) -> bool {

    }

    #[inline]
    fn lock_shared(&self) {
        if !self.try_lock_shared() {
            self.lock_shared_slow();
        }
    }

    #[inline]
    fn unlock_shared(&self) {
        
    }
}

impl<E: ThreadEvent> CoreRwLock<E> {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            queue: AtomicUsize::new(0),
            phantom: PhantomData,
        }
    }

    #[cold]
    fn lock_exclusive_slow(&self) {
        let node = WaitNode::<E, ()>::default();
        let mut state = self.state.load(Ordering::Relaxed);

        loop {
            
        }
    }

    #[cold]
    fn unlock_exclusive_slow(&self) {

    }

    #[cold]
    fn lock_shared_slow(&self) {

    }

    #[cold]
    fn unlock_shared_slow(&self) {

    }
}