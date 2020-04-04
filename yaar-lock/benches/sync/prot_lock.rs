#![allow(unused)]

use std::{
    fmt,
    mem::{replace, MaybeUninit},
    ptr::{null, NonNull},
    cell::{Cell, UnsafeCell},
    ops::{Deref, DerefMut},
    hint::unreachable_unchecked,
    sync::{
        Condvar,
        Mutex as StdMutex,
        atomic::{fence, spin_loop_hint, Ordering, AtomicU8, AtomicUsize},
    },
};

const UNLOCKED: usize = 0;
const LOCKED: usize   = 1;
const WAKING: usize   = 1 << 8;
const WAITING: usize  = 1 << 9;

struct Node {
    state: Cell<MaybeUninit<AtomicUsize>>,
    prev: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    next: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    tail: Cell<MaybeUninit<Option<NonNull<Self>>>>,
}

pub struct Mutex<T> {
    state: AtomicUsize,
    futex: AtomicUsize,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    #[inline]
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED),
            futex: AtomicUsize::new(UNLOCKED),
            value: UnsafeCell::new(value),
        }
    }

    fn byte_state(&self) -> &AtomicU8 {
        unsafe { &*(&self.state as *const _ as *const _) }
    }

    pub fn locked<U>(&self, f: impl FnOnce(&mut T) -> U) -> U {
        unsafe {
            if let Err(_) = self.byte_state().compare_exchange_weak(
                0,
                1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                self.lock_slow();
            }

            let result = f(&mut *self.value.get());

            self.byte_state().store(0, Ordering::Release);
            self.unlock_slow();

            result
        }
    }

    #[cold]
    unsafe fn lock_slow(&self) {
        let mut spin: usize = 0;
        let max_spin = Self::max_spin();
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state & LOCKED == 0 {
                if let Ok(_) = self.byte_state().compare_exchange_weak(
                    0,
                    1,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    return;
                }
                spin_loop_hint();
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            if (state & WAITING == 0) && spin < max_spin {
                spin += 1;
                spin_loop_hint();
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            if let Err(e) = self.state.compare_exchange_weak(
                state,
                state + WAITING,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                state = e;
                continue;
            }
            
            let mut wait_spin: usize = 0;
            loop {
                state = self.futex.load(Ordering::Relaxed);
                if state == LOCKED {
                    match self.futex.compare_exchange_weak(
                        LOCKED,
                        UNLOCKED,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => break,
                        Err(e) => state = e,
                    }
                    spin_loop_hint();
                    continue;
                }
                if wait_spin < max_spin {
                    wait_spin += 1;
                    spin_loop_hint();
                    state = self.futex.load(Ordering::Relaxed);
                    continue;
                }
                let _ = libc::syscall(
                    libc::SYS_futex,
                    &self.futex as *const _ as *const usize,
                    libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
                    UNLOCKED,
                    0,
                );
            }

            state = self.state.fetch_sub(WAKING, Ordering::Relaxed);
            state -= WAKING;
            spin = 0;
        }
    }

    #[cold]
    unsafe fn unlock_slow(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if (state < WAITING) || (state & LOCKED != 0) || (state & WAKING != 0) {
                return;
            }
            match self.state.compare_exchange_weak(
                state,
                (state - WAITING) + WAKING,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Err(e) => state = e,
                Ok(_) => break,
            }
        }

        self.futex.store(LOCKED, Ordering::Relaxed);
        let _ = libc::syscall(
            libc::SYS_futex,
            &self.futex as *const _ as *const u32,
            libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
            1,
        );
    }   

    fn max_spin() -> usize {
        #[cfg(target_arch = "x86")]
        use core::arch::x86::{__cpuid, CpuidResult};
        #[cfg(target_arch = "x86_64")]
        use core::arch::x86_64::{__cpuid, CpuidResult};

        use core::{
            slice::from_raw_parts,
            str::from_utf8_unchecked,
            hint::unreachable_unchecked,
        };

        static IS_AMD: AtomicUsize = AtomicUsize::new(0);
        let is_amd = unsafe {
            match IS_AMD.load(Ordering::Relaxed) {
                0 => {
                    let CpuidResult { ebx, ecx, edx, .. } = __cpuid(0);
                    let vendor = &[ebx, edx, ecx] as *const _ as *const u8;
                    let vendor = from_utf8_unchecked(from_raw_parts(vendor, 3 * 4));
                    let is_amd = vendor == "AuthenticAMD";
                    IS_AMD.store((is_amd as usize) + 1, Ordering::Relaxed);
                    is_amd
                },
                1 => false,
                2 => true,
                _ => unreachable_unchecked(),
            }
        };

        if is_amd { 2 } else { 40 } 
    }

    #[inline]
    pub unsafe fn force_unlock(&self) {
        unreachable!()
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