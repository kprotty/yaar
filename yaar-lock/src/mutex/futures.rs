use core::{
    mem,
    fmt,
    pin::Pin,
    future::Future,
    ptr::{null, NonNull},
    cell::{Cell, UnsafeCell},
    ops::{Drop, Deref, DerefMut},
    task::{Poll, Waker, Context},
    sync::atomic::{fence, spin_loop_hint, Ordering, AtomicUsize},
};

pub struct Mutex<T> {
    state: AtomicUsize,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> From<T> for Mutex<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: fmt::Debug> fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_lock() {
            Some(guard) => f.debug_struct("Mutex").field("data", &&*guard).finish(),
            None => {
                struct LockedPlaceholder;
                impl fmt::Debug for LockedPlaceholder {
                    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("<locked>")
                    }
                }

                f.debug_struct("Mutex")
                    .field("data", &LockedPlaceholder)
                    .finish()
            }
        }
    }
}

const MUTEX_LOCK: usize = 1 << 0;
const QUEUE_LOCK: usize = 1 << 1;
const QUEUE_MASK: usize = !(MUTEX_LOCK | QUEUE_LOCK);

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicUsize::new(0),
            value: UnsafeCell::new(value),
        }
    }

    pub fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value.get() }
    }

    pub fn is_locked(&self) -> bool {
        self.state.load(Ordering::Acquire) & MUTEX_LOCK != 0
    }

    #[inline]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.state
            .compare_exchange_weak(0, MUTEX_LOCK, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| MutexGuard{ mutex: self })
    }

    pub fn lock(&self) -> impl Future<Output = MutexGuard<'_, T>> {
        FutureLock {
            mutex: self,
            waker: Cell::new(None),
            prev: Cell::new(null()),
            next: Cell::new(null()),
            tail: Cell::new(null()),
        }
    }
}

struct FutureLock<'a, T> {
    mutex: &'a Mutex<T>,
    prev: Cell<*const Self>,
    next: Cell<*const Self>,
    tail: Cell<*const Self>,
    waker: Cell<Option<NonNull<Waker>>>,
}

impl<'a, T> Future for FutureLock<'a, T> {
    type Output = MutexGuard<'a, T>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match this.mutex.try_lock().or_else(|| this.lock_slow(context.waker())) {
            Some(guard) => Poll::Ready(guard),
            None => Poll::Pending,
        }
    }
}

impl<'a, T> FutureLock<'a, T> {
    #[cold]
    fn lock_slow(&self, waker: &'_ Waker) -> Option<MutexGuard<'a, T>> {
        const SPIN_COUNT_DOUBLING: usize = 4;

        let mut spin = 0;
        let mut state = self.mutex.state.load(Ordering::Relaxed);
        loop {
            // Anytime the mutex is unlocked, try to acquire it.
            if state & MUTEX_LOCK == 0 {
                match self.mutex.state.compare_exchange_weak(
                    state,
                    state | MUTEX_LOCK,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return Some(MutexGuard { mutex: self.mutex }),
                    Err(s) => state = s,
                }
                continue;
            }

            if (state & QUEUE_MASK == 0) && spin < SPIN_COUNT_DOUBLING {
                spin += 1;
                (0..(1 << spin)).for_each(|_| spin_loop_hint());
                state = self.mutex.state.load(Ordering::Relaxed);
                continue;
            }

            let head = (state & QUEUE_MASK) as *const Self;
            self.next.set(head);
            self.prev.set(null());
            self.waker.set(NonNull::new(waker as *const _ as *mut _));
            if head.is_null() {
                self.tail.set(self);
            } else {
                self.tail.set(null());
            }

            match self.mutex.state.compare_exchange_weak(
                state,
                (self as *const _ as usize) | (state & !QUEUE_MASK),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return None,
                Err(s) => state = s,
            }
        }
    }
}

pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
}

impl<'a, T> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        let state = self.mutex.state.fetch_sub(MUTEX_LOCK, Ordering::Release);
        if (state & QUEUE_LOCK == 0) && (state & QUEUE_MASK != 0) {
            unsafe { self.unlock_slow() };
        }
    }
}

impl<'a, T> MutexGuard<'a, T> {
    #[cold]
    unsafe fn unlock_slow(&self) {
        let mut state = self.mutex.state.load(Ordering::Relaxed);
        loop {
            if (state & QUEUE_LOCK != 0) || (state & QUEUE_MASK == 0) {
                return;
            }
            match self.mutex.state.compare_exchange_weak(
                state,
                state | QUEUE_LOCK,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(s) => state = s,
            }
        }

        'outer: loop {
            let head = &*((state & QUEUE_MASK) as *const FutureLock<'a, T>);
            let mut current = head;
            while current.tail.get().is_null() {
                let next = &*current.next.get();
                next.prev.set(current);
                current = next;
            }
            let tail = &*current.tail.get();
            head.tail.set(tail);

            if state & MUTEX_LOCK != 0 {
                match self.mutex.state.compare_exchange_weak(
                    state,
                    state & !QUEUE_LOCK,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(s) => state = s,
                }
                fence(Ordering::Acquire);
                continue;
            }

            let new_tail = tail.prev.get();
            if new_tail.is_null() {
                loop {
                    match self.mutex.state.compare_exchange_weak(
                        state,
                        state & MUTEX_LOCK,
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => break,
                        Err(s) => state = s,
                    }

                    if state & QUEUE_MASK != 0 {
                        fence(Ordering::Acquire);
                        continue 'outer;
                    }
                }

            } else {
                head.tail.set(new_tail);
                self.mutex.state.fetch_and(!QUEUE_LOCK, Ordering::Release);
            }

            let waker = mem::replace(&mut *tail.waker.as_ptr(), None);
            let waker = &* waker.unwrap().as_ptr();
            waker.wake_by_ref();
            return;
        }
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
