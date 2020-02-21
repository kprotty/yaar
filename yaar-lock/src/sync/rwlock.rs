use crate::ThreadEvent;
use super::{WaitNode, SpinWait, CoreMutex};
use core::{
    ptr::null,
    cell::Cell,
    sync::atomic::{Ordering, AtomicUsize},
};
use lock_api::RawMutex;

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

#[derive(Copy, Clone, PartialEq)]
enum Tag {
    Reader,
    Writer,
}

struct MutexLockGuard<'a, E: ThreadEvent> {
    mutex: &'a CoreMutex<E>,
}

impl<'a, E: ThreadEvent> MutexLockGuard<'a, E> {
    fn new(mutex: &'a CoreMutex<E>) -> Self {
        mutex.lock();
        Self { mutex }
    }
}

impl<'a, E: ThreadEvent> Drop for MutexLockGuard<'a, E> {
    fn drop(&mut self) {
        self.mutex.unlock();
    }
}

pub struct CoreRwLock<E: ThreadEvent> {
    state: AtomicUsize,
    mutex: CoreMutex<E>,
    queue: Cell<*const WaitNode<E, Tag>>,
}

unsafe impl<E: ThreadEvent> Send for CoreRwLock<E> {}
unsafe impl<E: ThreadEvent> Sync for CoreRwLock<E> {}

unsafe impl<E: ThreadEvent> lock_api::RawRwLock for CoreRwLock<E> {
    const INIT: Self = Self {
        state: AtomicUsize::new(0),
        mutex: CoreMutex::INIT,
        queue: Cell::new(null()),
    };

    type GuardMarker = lock_api::GuardSend;

    #[inline]
    fn try_lock_exclusive(&self) -> bool {
        self.state
            .compare_exchange(0, WRITE, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    #[inline]
    fn lock_exclusive(&self) {
        if self
            .state
            .compare_exchange_weak(0, WRITE, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
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
        self.try_lock_shared_fast() || self.try_lock_shared_slow()
    }

    #[inline]
    fn lock_shared(&self) {
        if !self.try_lock_shared_fast() {
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
        self.lock_common(Tag::Writer, |state: &mut usize| {
            loop {
                if *state & WRITE != 0 {
                    return false;
                }
                match self.state.compare_exchange_weak(
                    *state,
                    *state | WRITE,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return true,
                    Err(s) => *state = s,
                }
            }
        })
    }

    #[cold]
    fn unlock_exclusive_slow(&self) {
        let node = {
            let _lock = MutexLockGuard::new(&self.mutex);
            let head = self.queue.get();
            if head.is_null() {
                self.state.store(0, Ordering::Release);
                return;
            }

            let head = unsafe { &*head };
            let tail = head.tail();
            let new_tail = tail.next();
            head.pop(new_tail);

            if new_tail.is_null() {
                self.queue.set(null());
                self.state.store(0, Ordering::Release);
            } else {
                self.state.store(PARKED, Ordering::Release);
            }

            tail
        };

        node.notify();
    }

    #[inline(always)]
    fn try_lock_shared_fast(&self) -> bool {
        let state = self.state.load(Ordering::Relaxed);
        if state & (WRITE | PARKED) == 0 {
            if let Some(new_state) = state.checked_add(READ) {
                return self
                    .state
                    .compare_exchange_weak(state, new_state, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok();
            }
        }
        false
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
        self.lock_common(Tag::Reader, |state: &mut usize| {
            let mut spin_wait = SpinWait::new();
            loop {
                if *state & WRITE != 0 {
                    return false;
                }
                
                if self
                    .state
                    .compare_exchange_weak(
                        *state,
                        state.checked_add(READ)
                            .expect("RwLock reader count overflow"),
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    return true;
                }

                spin_wait.force_spin();
                *state = self.state.load(Ordering::Relaxed);
            }
        });
    }

    #[cold]
    fn unlock_shared_slow(&self) {
        unimplemented!()
    }

    fn lock_common(
        &self,
        tag: Tag,
        try_lock: impl Fn(&mut usize) -> bool,
    ) {
        let mut spin_wait = SpinWait::new();
        let wait_node = WaitNode::<E, Tag>::new(tag);
        let mut state = self.state.load(Ordering::Relaxed);

        loop {
            if try_lock(&mut state) {
                return;
            }

            if state & PARKED == 0 {
                if spin_wait.spin() {
                    state = self.state.load(Ordering::Relaxed);
                    continue;
                } else if let Err(s) = self.state.compare_exchange_weak(
                    state,
                    state | PARKED,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    state = s;
                    continue;
                }
            }
            
            let should_park = {
                let _lock = MutexLockGuard::new(&self.mutex);
                let state = self.state.load(Ordering::Relaxed);
                if state & (WRITE | PARKED) == 0 {
                    false
                } else {
                    let head = self.queue.get();
                    let new_head = wait_node.push(head);
                    self.queue.set(new_head);
                    true
                }
            };

            if should_park {
                wait_node.wait();
                wait_node.reset();
            }
            spin_wait.reset();
            state = self.state.load(Ordering::Relaxed);
        }
    }
}
