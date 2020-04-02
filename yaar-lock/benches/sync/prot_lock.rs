#![allow(unused)]

use std::{
    fmt,
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    sync::atomic::{spin_loop_hint, Ordering, AtomicU32},
};

const UNLOCKED:  u32 = 0 << 0;
const LOCKED:    u32 = 1 << 0;
const WAKING:    u32 = 1 << 1;
const CONTENDED: u32 = 1 << 1;

pub struct Mutex<T> {
    state: AtomicU32,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    #[inline]
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicU32::new(UNLOCKED),
            value: UnsafeCell::new(value),
        }
    }

    #[inline]
    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value.get() }
    }

    #[inline]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        let state = self.state.fetch_and(LOCKED, Ordering::Acquire);
        if state & LOCKED == 0 {
            Some(MutexGuard { mutex: self })
        } else {
            None
        }
    }

    #[inline]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        let state = self.state.fetch_and(LOCKED, Ordering::Acquire);
        if state & LOCKED != 0 {
            self.lock_slow(state);
        }
        MutexGuard{ mutex: self }
    }

    #[cold]
    fn lock_slow(&self, mut state: u32) {
        let mut spin = 0;
        loop {
            if state & LOCKED == 0 {
                state = self.state.fetch_and(LOCKED, Ordering::Acquire);
                if state & LOCKED == 0 {
                    return;
                }
                continue;
            }

            if state & CONTENDED == 0 {
                if spin < 40 {
                    spin_loop_hint();                        
                    spin += 1;
                    state = self.state.load(Ordering::Relaxed);
                    continue;
                }

                if let Err(e) = self.state.compare_exchange_weak(
                    state,
                    state | CONTENDED,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    state = e;
                    continue;
                }

                state |= CONTENDED;
            }

            let _ = unsafe {
                libc::syscall(
                    libc::SYS_futex,
                    &self.state as *const _ as *const u32,
                    libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
                    state,
                    0,
                )
            };

            state = self.state.load(Ordering::Relaxed);
            if state & WAKING == 0 {
                continue;
            }
            
            state = self.state.fetch_and(!WAKING, Ordering::Relaxed);
            if state & WAKING != 0 {
                spin = 0;
            }
        }
    }

    #[inline]
    pub unsafe fn force_unlock(&self) {
        let state = self.state.fetch_and(!LOCKED, Ordering::Release);
        if state & (CONTENDED | WAKING) == CONTENDED {
            self.unlock_slow()
        }
    }

    #[cold]
    fn unlock_slow(&self) {
        let state = self.state.fetch_and(WAKING, Ordering::Relaxed);
        if state & WAKING == 0 {
            let _ = unsafe {
                libc::syscall(
                    libc::SYS_futex,
                    &self.state as *const _ as *const u32,
                    libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
                    1,
                )
            };
        }
    }
}

pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
}

impl<'a, T> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        unsafe { self.mutex.force_unlock() };
    }
}

impl<'a, T> DerefMut for MutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.value.get() }
    }
}

impl<'a, T> Deref for MutexGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.mutex.value.get() }
    }
}

impl<'a, T: fmt::Debug> fmt::Debug for MutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<'a, T: fmt::Display> fmt::Display for MutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}