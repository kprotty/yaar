use std::cell::UnsafeCell;

pub struct Lock<T> {
    value: UnsafeCell<T>,
    inner: os::Lock,
}

unsafe impl<T: Send> Send for Lock<T> {}
unsafe impl<T: Send> Sync for Lock<T> {}

impl<T> Lock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            value: UnsafeCell::new(value),
            inner: os::Lock::new(),
        }
    }

    pub fn with<F>(&self, f: impl FnOnce(&mut T) -> F) -> F {
        self.inner.with(|| f(unsafe { &mut *self.value.get() }))
    }
}

#[cfg(target_os = "windows")]
mod os {
    use std::{cell::UnsafeCell, ffi::c_void, ptr};

    #[link(name = "kernel32")]
    extern "system" {
        fn AcquireSRWLockExclusive(p: *mut *mut c_void);
        fn ReleaseSRWLockExclusive(p: *mut *mut c_void);
    }

    pub struct Lock(UnsafeCell<*mut c_void>);

    impl Lock {
        pub const fn new() -> Self {
            Self(UnsafeCell::new(ptr::null_mut()))
        }

        pub unsafe fn with<F>(&self, f: impl FnOnce() -> F) -> F {
            AcquireSRWLockExclusive(self.0.get());
            let result = f();
            ReleaseSRWLockExclusive(self.0.get());
            result
        }
    }
}

#[cfg(target_vendor = "apple")]
mod os {
    use std::{cell::UnsafeCell};

    #[link(name = "c")]
    extern "C" {
        fn os_unfair_lock_lock(p: *mut u32);
        fn os_unfair_lock_unlock(p: *mut u32);
    }

    pub struct Lock(UnsafeCell<*mut c_void>);

    impl Lock {
        pub const fn new() -> Self {
            Self(UnsafeCell::new(0))
        }

        pub unsafe fn with<F>(&self, f: impl FnOnce() -> F) -> F {
            os_unfair_lock_lock(self.0.get());
            let result = f();
            os_unfair_lock_unlock(self.0.get());
            result
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod os {
    use std::{hint::spin_loop as spin_loop_hint, ptr, sync::atomic::{AtomicI32, Ordering}};

    const UNLOCKED: i32 = 0;
    const LOCKED: i32 = 1;
    const CONTENDED: i32 = 2;

    pub struct Lock {
        state: AtomicI32,
    }

    impl Lock {
        pub const fn new() -> Self {
            Self {
                state: AtomicI32::new(UNLOCKED),
            }
        }

        pub unsafe fn with<F>(&self, f: impl FnOnce() -> F) -> F {
            self.acquire();
            let result = f();
            self.release();
            result
        }

        #[cold]
        unsafe fn acquire(&self) {
            let mut lock_state = match self.state.swap(LOCKED, Ordering::Acquire) {
            if lock_state == UNLOCKED {
                return;
            }

            let mut spin = 100;
            let mut state = LOCKED;
            loop {
                if state == UNLOCKED {
                    match self.state.compare_exchange(
                        state,
                        lock_state,
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => return,
                        Err(e) => state = e,
                    }
                }

                if state != CONTENDED {
                    debug_assert_eq!(state, LOCKED);

                    if let Some(new_spin) = spin.checked_sub(1) {
                        spin = new_spin;
                        spin_loop_hint();
                        state = self.state.load(Ordering::Relaxed);
                        continue;
                    }

                    state = self.state.swap(CONTENDED, Ordering::Acquire);
                    if state == UNLOCKED {
                        return;
                    } else {
                        debug_assert!(state == LOCKED || state == CONTENDED);
                    }
                }

                let _ = libc::syscall(
                    libc::SYS_futex,
                    &self.state as *const AtomicI32,
                    libc::FUTEX_PRIVATE_FLAG | libc::FUTEX_WAIT,
                    CONTENDED,
                    ptr::null::<libc::timespec>(),
                );

                spin = 0;
                lock_state = CONTENDED;
                state = self.state.load(Ordering::Relaxed);
            }
        }

        #[cold]
        unsafe fn release(&self) {
            let state = self.state.swap(UNLOCKED, Ordering::Release);
            debug_assert!(state == LOCKED || state == CONTENDED);

            if state == CONTENDED {
                let _ = libc::syscall(
                    libc::SYS_futex,
                    &self.state as *const AtomicI32,
                    libc::FUTEX_PRIVATE_FLAG | libc::FUTEX_WAKE,
                    1i32,
                );
            }
        }
    }
}

#[cfg(all(
    unix,
    not(target_vendor = "apple"),
    not(target_os = "linux"),
    not(target_os = "android"),
))]
mod os {
    use super::super::Event;
    use std::{cell::{Cell, UnsafeCell}, marker::PhantomPinned, pin::Pin, ptr::NonNull, hint::spin_loop as spin_loop_hint, sync::atomic::{fence, AtomicUsize, Ordering}};

    const UNLOCKED: usize = 0;
    const LOCKED: usize = 1;
    const WAKING: usize = 2;
    const WAITING: usize = !3;

    #[derive(Default)]
    #[repr(align(4))]
    struct Waiter {
        event: Cell<Event>,
        prev: Cell<Option<NonNull<Self>>>,
        next: Cell<Option<NonNull<Self>>>,
        tail: Cell<Option<NonNull<Self>>>,
        _pinned: PhantomPinned,
    }

    pub struct Lock {
        state: AtomicUsize,
    }

    impl Lock {
        pub const fn new() -> Self {
            Self {
                state: AtomicUsize::new(UNLOCKED),
            }
        }

        pub unsafe fn with<F>(&self, f: impl FnOnce() -> F) -> F {
            self.acquire();
            let result = f();
            self.release();
            result
        }

        #[cold]
        fn acquire(&self) {
            let mut state = match self.state.compare_exchange_weak(
                UNLOCKED,
                LOCKED,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(e) => e,
            };

            let mut spin = 100;
            let waiter = Waiter::default();
            let waiter = unsafe { Pin::new_unchecked(&waiter) };
            loop {
                if state & LOCKED == 0 {
                    match self.state.compare_exchange_weak(
                        state,
                        state | LOCKED,
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => return,
                        Err(e) => state = e,
                    }
                    continue;
                }

                let head = NonNull::new((state & WAITING) as *mut Waiter);
                if head.is_none() && spin > 0 {
                    spin -= 1;
                    spin_loop_hint();
                    state = self.state.load(Ordering::Relaxed);
                    continue;
                }
                
                waiter.event.set(Event::default());
                waiter.prev.set(None);
                waiter.next.set(head);
                waiter.tail.set(match head {
                    Some(_) => None,
                    None => Some(NonNull::from(&*waiter)),
                });

                if let Err(e) = self.state.compare_exchange_weak(
                    state,
                    (state & !WAITING) | (&*waiter as *const Waiter as usize),
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    state = e;
                    continue;
                }

                debug_assert!(waiter.event.wait(None));
                state = self.state.load(Ordering::Relaxed);
                spin = 0;
            }
        }

        #[cold]
        unsafe fn release(&self) {
            let mut state = self.state.fetch_sub(LOCKED, Ordering::Release);
            if (state & WAITING == 0) || (state & WAKING != 0) {
                return;
            }

            state -= LOCKED;
            loop {
                match self.state.compare_exchange_weak(
                    state,
                    state | WAKING,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(e) => state = e,
                }
                if (state & WAITING == 0) || (state & (LOCKED | WAKING) != 0) {
                    return;
                }
            }

            state |= WAKING;
            'dequeue: loop {
                let head = NonNull::new((state & WAITING) as *mut Waiter).unwrap();
                let tail = head.as_ref().tail.get().unwrap_or_else(|| {
                    let mut current = head;
                    loop {
                        let next = current.as_ref().next.get().unwrap();
                        next.as_ref().prev.set(Some(current));
                        current = next;
                        
                        if let Some(tail) = current.as_ref().tail.get() {
                            head.as_ref().tail.set(Some(tail));
                            break tail;
                        }
                    }
                });

                if state & LOCKED != 0 {
                    match self.state.compare_exchange_weak(
                        state,
                        state & !WAKING,
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => return,
                        Err(e) => state = e,
                    }

                    debug_assert!(state & WAKING != 0);
                    fence(Ordering::Acquire);
                    continue;
                }

                match tail.as_ref().prev.get() {
                    Some(new_tail) => {
                        debug_assert_eq!(head.as_ref().tail.get(), tail);
                        head.as_ref().tail.set(Some(new_tail));
                        
                        state = self.state.fetch_sub(WAKING, Ordering::Release);
                        debug_assert!(state & WAKING != 0);
                    },
                    None => loop {
                        match self.state.compare_exchange_weak(
                            state,
                            state & LOCKED,
                            Ordering::Release,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => break,
                            Err(e) => state = e,
                        }

                        if state & WAITING != 0 {
                            debug_assert!(state & WAKING != 0);
                            fence(Ordering::Acquire);
                            continue 'dequeue;
                        }
                    }
                }

                tail.as_ref().event.notify();
                return;
            }
        }
    }
}