use core::{
    cell::{Cell, UnsafeCell},
    fmt,
    future::Future,
    mem::{self, MaybeUninit},
    ops::{Deref, DerefMut, Drop},
    pin::Pin,
    ptr::null,
    sync::atomic::{fence, spin_loop_hint, AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};

const MUTEX_LOCK: usize = 1 << 0;
const QUEUE_LOCK: usize = 1 << 1;
const QUEUE_MASK: usize = !(MUTEX_LOCK | QUEUE_LOCK);

struct WordLock {
    state: AtomicUsize,
}

impl WordLock {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
        }
    }

    /// Try to lock the mutex without blocking or
    /// enqueuing into the wait queue.
    #[inline]
    pub fn try_lock(&self) -> bool {
        self.state
            .compare_exchange_weak(0, MUTEX_LOCK, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    /// Try to lock the mutex without blocking, returning true 
    /// if it was successful, or false if it is locked/contended
    /// and if the given node/waker is pushed onto the wait queue.
    #[inline]
    pub fn lock(&self, node: &WaitNode, waker: &Waker) -> bool {
        (node.flags.get() & DIRECT_HANDOFF != 0) || 
            self.try_lock() || 
            self.lock_slow(node, waker)
    }

    /// Tries to lock the mutex without blocking. 
    /// 
    /// If the mutex remains locked, then we add ourselves
    /// to the queue of waiting futures, clone the waker,
    /// and return false.
    #[cold]
    fn lock_slow(&self, node: &WaitNode, waker: &Waker) -> bool {
        const SPIN_COUNT_DOUBLING: usize = 4;

        let mut spin = 0;
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            // try to lock the mutex if its unlocked
            if state & MUTEX_LOCK == 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state | MUTEX_LOCK,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return true,
                    Err(s) => state = s,
                }
                continue;
            }

            // spin a bit if there arent any waiting futures & we haven't spun too much
            if (state & QUEUE_MASK == 0) && spin < SPIN_COUNT_DOUBLING {
                spin += 1;
                (0..(1 << spin)).for_each(|_| spin_loop_hint());
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            // prepare the node to be added to the queue.
            node.prepare((state & QUEUE_MASK) as *const _, waker);

            // Try to push ourselves to the head of the queue
            match self.state.compare_exchange_weak(
                state,
                (node as *const _ as usize) | (state & !QUEUE_MASK),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return false,
                Err(s) => state = s,
            }
        }
    }

    /// Unlock the queue in an unfair manner.
    ///
    /// This means that the current thread has the ability
    /// to acquire the lock again immediately even if there
    /// are WaitNodes waiting the queue.
    #[inline]
    pub unsafe fn unlock_unfair(&self) {
        let state = self.state.fetch_sub(MUTEX_LOCK, Ordering::Release);
        if (state & QUEUE_LOCK == 0) && (state & QUEUE_MASK != 0) {
            self.unlock_unfair_slow();
        }
    }

    #[cold]
    unsafe fn unlock_unfair_slow(&self) {
        // first, try to lock the queue
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if (state & QUEUE_LOCK != 0) || (state & QUEUE_MASK == 0) {
                return;
            }
            match self.state.compare_exchange_weak(
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
            // then find the tail node
            let head = &*((state & QUEUE_MASK) as *const WaitNode);
            let tail = head.find_tail();

            // if the queue is already locked, let the unlocker future
            // handling popping of a node from the queue to wake it.
            if state & MUTEX_LOCK != 0 {
                match self.state.compare_exchange_weak(
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

            // pop the tail node off the queue by replacing the
            // tail its the tail's previous link + unlocking the queue.
            let new_tail = tail.prev.get().assume_init();
            if new_tail.is_null() {
                loop {
                    match self.state.compare_exchange_weak(
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
                head.tail.set(MaybeUninit::new(new_tail));
                self.state.fetch_and(!QUEUE_LOCK, Ordering::Release);
            }

            // Wake up the waker registered to the tail node we just popped.
            tail.wake();
            return;
        }
    }
}

const HAS_WAKER: u8 = 1 << 0;
const DIRECT_HANDOFF: u8 = 1 << 1;

struct WaitNode {
    flags: Cell<u8>,
    waker: Cell<MaybeUninit<Waker>>,
    prev: Cell<MaybeUninit<*const Self>>,
    next: Cell<MaybeUninit<*const Self>>,
    tail: Cell<MaybeUninit<*const Self>>,
}

// Use `MaybeUninit` to lazy initialize for lock/unlock fast path.
// This should avoid writing memory (excluding flags) and only do the stack allocation.
impl Default for WaitNode {
    fn default() -> Self {
        Self {
            flags: Cell::new(0),
            waker: Cell::new(MaybeUninit::uninit()),
            prev: Cell::new(MaybeUninit::uninit()),
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
        }
    }
}

// Only drop the waker if its set
impl Drop for WaitNode {
    fn drop(&mut self) {
        if self.flags.get() & HAS_WAKER != 0 {
            unsafe {
                let waker = mem::replace(&mut *self.waker.as_ptr(), MaybeUninit::uninit());
                mem::drop(waker.assume_init());
            }
        }
    }
}

impl WaitNode {
    /// Prepare the wait node to be pushed to the queue
    /// using the queue head and its waker to wake once
    /// popped from the queue.
    ///
    /// Returns whether the node is already in the queue.
    #[inline]
    pub fn prepare(&self, head: *const Self, waker: &Waker) {
        // lazily initialize the waker only once as clone() could be expensive.
        let flags = self.flags.get();
        if flags & HAS_WAKER == 0 {
            self.flags.set(flags | HAS_WAKER);
            self.prev.set(MaybeUninit::new(null()));
            self.waker.set(MaybeUninit::new(waker.clone()));
        }

        // update the node link pointers based on the head node 
        self.next.set(MaybeUninit::new(head));
        if head.is_null() {
            self.tail.set(MaybeUninit::new(self));
        } else {
            self.tail.set(MaybeUninit::new(null()));
        }
    }

    /// Starting from the head of the queue as ourselves,
    /// try to find the tail of the queue by following the queue
    /// linked list until we see a node with the tail.
    ///
    /// Then, we cache tail on ourself so that later `find_tail()`
    /// calls will have less of the linked list to traverse.
    #[inline]
    pub unsafe fn find_tail<'a>(&self) -> &'a Self {
        let mut current = self;
        loop {
            let tail = current.tail.get().assume_init();
            if tail.is_null() {
                let next = &*current.next.get().assume_init();
                next.prev.set(MaybeUninit::new(current));
                current = next;
            } else {
                self.tail.set(MaybeUninit::new(tail));
                return &*tail;
            }
        }
    }

    /// Wake up the current waker stored
    /// in this wait node, consuming it in the process.
    #[inline]
    pub fn wake(&self) {
        #[cfg(debug_assertions)] {
            let flags = self.flags.get();
            debug_assert!(flags & HAS_WAKER != 0);
            self.flags.set(flags & !HAS_WAKER);
        }
        unsafe {
            let waker = mem::replace(&mut *self.waker.as_ptr(), MaybeUninit::uninit());
            waker.assume_init().wake();
        }
    }
}

/// Future aware mutex based on [`WordLock`] from parking_lot.
///
/// [`WordLock`]: https://github.com/Amanieu/parking_lot/blob/master/core/src/word_lock.rs
pub struct Mutex<T> {
    lock: WordLock,
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

// doc comments copied
// from: https://docs.rs/lock_api/0.3.3/lock_api/struct.Mutex.html
// and: https://docs.rs/futures-intrusive/0.2.2/futures_intrusive/sync/struct.GenericMutex.html
impl<T> Mutex<T> {
    /// Creates a new mutex in an unlocked state ready for use.
    pub const fn new(value: T) -> Self {
        Self {
            lock: WordLock::new(),
            value: UnsafeCell::new(value),
        }
    }

    /// Consumes this mutex, returning the underlying data.
    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the `Mutex` mutably, no actual locking needs to
    /// take place---the mutable borrow statically guarantees no locks exist.
    pub fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value.get() }
    }

    /// Forcibly unlocks the mutex.
    ///
    /// This is useful when combined with `mem::forget` to hold a lock without
    /// the need to maintain a `MutexGuard` object alive, for example when
    /// dealing with FFI.
    ///
    /// # Safety
    ///
    /// This method must only be called if the current thread logically owns a
    /// `MutexGuard` but that guard has be discarded using `mem::forget`.
    /// Behavior is undefined if a mutex is unlocked when not locked.
    #[inline]
    pub unsafe fn force_unlock(&self) {
        self.lock.unlock_unfair() 
    }

    /// Attempts to acquire this lock.
    ///
    /// If the lock could not be acquired at this time, then `None` is returned.
    /// Otherwise, an RAII guard is returned. The lock will be unlocked when the
    /// guard is dropped.
    ///
    /// This function does not block.
    #[inline]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        if self.lock.try_lock() {
            Some(MutexGuard{ mutex: self })
        } else {
            None
        }
    }

    /// Acquire the mutex asynchronously.
    ///
    /// Returns a future that will resolve once the mutex has been successfully acquired.
    /// TODO: Ensure !Unpin for the returned future.
    pub fn lock(&self) -> impl Future<Output = MutexGuard<'_, T>> {
        struct FutureLock<'a, T> {
            mutex: &'a Mutex<T>,
            node: WaitNode,
        }

        impl<'a, T> Future for FutureLock<'a, T> {
            type Output = MutexGuard<'a, T>;

            fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
                let this = self.as_ref();
                if this.mutex.lock.lock(&this.node, ctx.waker()) {
                    Poll::Ready(MutexGuard { mutex: this.mutex })
                } else {
                    Poll::Pending
                }
            }
        }

        FutureLock {
            mutex: self,
            node: WaitNode::default(),
        }
    }
}

/// An RAII implementation of a "scoped lock" of a mutex. When this structure is
/// dropped (falls out of scope), the lock will be unlocked.
///
/// The data protected by the mutex can be accessed through this guard via its
/// `Deref` and `DerefMut` implementations.
pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
}

impl<'a, T> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        unsafe { self.mutex.lock.unlock_unfair() };
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

impl<'a, T> MutexGuard<'a, T> {
    // TODO: 
    // - unlock_fair()
    // - bump()
    // - unlocked_fair(f)
    // - map
    // - try_map
}
