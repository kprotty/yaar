use super::ThreadParker;
use crate::shared::{WaitNode, WordLock, WAIT_NODE_INIT};
use core::marker::PhantomData;

/// RawMutex implementation which uses the default OS implementation for thread parking
#[cfg(feature = "os")]
pub type Mutex<T> = RawMutex<T, super::OsThreadParker>;

/// MutexGuard for [`Mutex`].
#[cfg(feature = "os")]
pub type MutexGuard<'a, T> = RawMutexGuard<'a, T, super::OsThreadParker>;

/// Mutex abstract which utilizes [`WordLock`] from parking_lot
/// in order to implement a fair lock which is only a `usize` large.
/// This is platform agnostic and requires the user to provide their
/// own method for blocking the current thread via [`ThreadParker`].
///
/// [`WordLock`]: https://github.com/Amanieu/parking_lot/blob/master/core/src/word_lock.rs
pub type RawMutex<T, Parker> = lock_api::Mutex<WordMutex<Parker>, T>;

/// MutexGuard for [`RawMutex`].
pub type RawMutexGuard<'a, T, Parker> = lock_api::MutexGuard<'a, WordMutex<Parker>, T>;

#[doc(hidden)]
pub struct WordMutex<Parker> {
    lock: WordLock,
    phantom: PhantomData<Parker>,
}

impl<Parker> WordMutex<Parker> {
    pub const fn new() -> Self {
        Self {
            lock: WordLock::new(),
            phantom: PhantomData,
        }
    }
}

impl<Parker: ThreadParker> WordMutex<Parker> {
    #[inline]
    fn acquire(&self, node: &WaitNode<Parker>) {
        const SPIN_COUNT_DOUBLING: usize = 4;
        loop {
            if self
                .lock
                .lock(SPIN_COUNT_DOUBLING, node, || Parker::default())
            {
                return;
            } else {
                let parker = Self::get_parker(node);
                parker.park();
                parker.reset();
            }
        }
    }

    #[inline]
    fn get_parker(node: &WaitNode<Parker>) -> &mut Parker {
        unsafe { &mut *(&mut *node.waker.as_ptr()).as_mut_ptr() }
    }

    #[inline]
    fn wake_node(node: &WaitNode<Parker>) {
        node.flags.set(node.flags.get() & !WAIT_NODE_INIT);
        Self::get_parker(node).unpark();
    }
}

unsafe impl<Parker: ThreadParker> lock_api::RawMutex for WordMutex<Parker> {
    const INIT: Self = Self::new();

    type GuardMarker = lock_api::GuardSend;

    fn try_lock(&self) -> bool {
        self.lock.try_lock()
    }

    fn lock(&self) {
        let node = WaitNode::<Parker>::default();
        self.acquire(&node);
    }

    fn unlock(&self) {
        if let Some(node) = self.lock.unlock_unfair() {
            Self::wake_node(node);
        }
    }
}

unsafe impl<Parker: ThreadParker> lock_api::RawMutexFair for WordMutex<Parker> {
    fn unlock_fair(&self) {
        if let Some(node) = self.lock.unlock_fair() {
            Self::wake_node(node);
        }
    }

    fn bump(&self) {
        let node = WaitNode::<Parker>::default();
        if let Some(waiting_node) = self.lock.bump(&node, || Parker::default()) {
            Self::wake_node(waiting_node);
            self.acquire(&node);
        }
    }
}
