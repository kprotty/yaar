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
        atomic::{fence, spin_loop_hint, Ordering, AtomicU8, AtomicU32, AtomicUsize},
    },
};

const UNLOCKED: usize = 0;
const LOCKED: usize   = 1;
const WAKING: usize   = 1 << 8;
const MASK: usize = (!(WAKING - 1)) & !WAKING;

#[repr(align(512))]
struct Node {
    state: AtomicUsize,
    prev: Cell<Option<NonNull<Self>>>,
    next: Cell<Option<NonNull<Self>>>,
    tail: Cell<Option<NonNull<Self>>>,
}

pub struct Mutex<T> {
    state: AtomicUsize,
    head: Cell<Option<NonNull<Node>>>,
    tail: AtomicUsize,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    #[inline]
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED),
            head: Cell::new(None),
            tail: AtomicUsize::new(0),
            value: UnsafeCell::new(value),
        }
    }

    fn byte_state(&self) -> &AtomicU8 {
        unsafe { &*(&self.state as *const _ as *const _) }
    }


    pub fn locked<U>(&self, f: impl FnOnce(&mut T) -> U) -> U {
        unsafe {
            if let Err(_) = self.byte_state().compare_exchange_weak(
                UNLOCKED as u8,
                LOCKED as u8,
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
        let mut spin = 0;
        let max_spin = Self::max_spin();
        let node = Node {
            state: AtomicUsize::new(UNLOCKED),
            prev: Cell::new(None),
            next: Cell::new(None),
            tail: Cell::new(None),
        };
        let mut state = self.state.load(Ordering::Relaxed);

        loop {
            if state & LOCKED == 0 {
                if let Ok(_) = self.byte_state().compare_exchange_weak(
                    UNLOCKED as u8,
                    LOCKED as u8,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    return;
                }
                spin_loop_hint();
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            let head = NonNull::new((state & MASK) as *mut Node);
            if head.is_none() && spin < max_spin {
                spin += 1;
                spin_loop_hint();
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            node.next.set(head);
            node.tail.set(match head {
                Some(_) => None,
                None => NonNull::new(&node as *const _ as *mut _),
            });

            if let Err(e) = self.state.compare_exchange_weak(
                state,
                (&node as *const _ as usize) | (state & !MASK),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                state = e;
                continue;
            }

            if node.state.load(Ordering::Relaxed) == UNLOCKED {
                if let Ok(_) = node.state.compare_exchange(
                    UNLOCKED,
                    WAKING,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    while node.state.load(Ordering::Acquire) == WAKING {
                        let _ = libc::syscall(
                            libc::SYS_futex,
                            &node.state as *const _ as *const u32,
                            libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
                            WAKING,
                            0,
                        );
                    }
                }
            }            

            spin = 0;
            node.prev.set(None);
            node.state.store(UNLOCKED, Ordering::Relaxed);
            state = self.state.fetch_sub(WAKING, Ordering::Relaxed) - WAKING;
        }        
    }

    #[cold]
    unsafe fn unlock_slow(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if ((state & MASK == 0) && self.tail.load(Ordering::Relaxed) == 0)
                || (state & LOCKED != 0)
                || (state & WAKING != 0) 
            {
                return;
            }
            match self.state.compare_exchange_weak(
                state,
                (state & LOCKED) | WAKING,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Err(e) => state = e,
                Ok(_) => break,
            }
        }

        if let Some(head) = NonNull::new((state & MASK) as *mut Node) {
            let head = &*head.as_ptr();
            let tail = {
                let mut current = head;
                loop {
                    if let Some(tail) = current.tail.get() {
                        break &*tail.as_ptr();
                    } else {
                        let next = &*current.next.get().unwrap_or_else(|| unreachable_unchecked()).as_ptr();
                        next.prev.set(NonNull::new(current as *const _ as *mut _));
                        current = next;
                    }
                }
            };

            if let Some(qhead) = self.head.get() {
                tail.next.set(Some(qhead));
                (&*qhead.as_ptr()).prev.set(NonNull::new(tail as *const _ as *mut _));
            } else {
                self.tail.store(tail as *const _ as usize, Ordering::Relaxed);
            }
            self.head.set(NonNull::new(head as *const _ as *mut _));
        }

        let tail = &*(self.tail.load(Ordering::Acquire) as *const Node);
        if let Some(new_tail) = tail.prev.get() {
            self.tail.store(new_tail.as_ptr() as usize, Ordering::Release);
        } else {
            self.head.set(None);
            self.tail.store(0, Ordering::Release);
        }
        
        if tail.state.load(Ordering::Relaxed) == UNLOCKED {
            if let Ok(_) = tail.state.compare_exchange(
                UNLOCKED,
                LOCKED,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                return;
            }
        }

        tail.state.store(UNLOCKED, Ordering::Release);
        let _ = libc::syscall(
            libc::SYS_futex,
            &tail.state as *const _ as *const u32,
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

        if is_amd { 0 } else { 40 } 
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