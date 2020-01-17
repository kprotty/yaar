// Contains lots of code lifted from futures-intrusive, parking_lot, and lock_api.
// Major credit to those crates for influencing this one.

use core::{
    cell::{Cell, UnsafeCell},
    fmt,
    future::Future,
    marker::PhantomPinned,
    mem::{self, align_of, MaybeUninit},
    ops::{Deref, DerefMut},
    pin::Pin,
    ptr::null,
    sync::atomic::{fence, AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};

/// A mutual exclusion primitive useful for protecting shared data
///
/// This mutex will block futures waiting for the lock to become available. The
/// mutex can also be statically initialized or created via a `new`
/// constructor. Each mutex has a type parameter which represents the data that
/// it is protecting. The data can only be accessed through the RAII guards
/// returned from `lock` and `try_lock`, which guarantees that the data is only
/// ever accessed when the mutex is locked.
pub struct Mutex<T> {
    value: UnsafeCell<T>,
    state: AtomicUsize,
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

impl<T> Mutex<T> {
    /// Creates a new futures-aware mutex.
    pub const fn new(value: T) -> Self {
        Self {
            value: UnsafeCell::new(value),
            state: AtomicUsize::new(0),
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

    /// Tries to acquire the mutex
    ///
    /// If acquiring the mutex is successful, a [`MutexGuard`]
    /// will be returned, which allows to access the contained data.
    ///
    /// Otherwise `None` will be returned.
    #[inline]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        if try_acquire(&self.state) {
            Some(MutexGuard { mutex: self })
        } else {
            None
        }
    }

    /// Acquire the mutex asynchronously.
    ///
    /// This method returns a future that will resolve once the mutex has been
    /// successfully acquired.
    ///
    #[inline]
    pub fn lock(&self) -> impl Future<Output = MutexGuard<'_, T>> {
        struct FutureLock<'a, T> {
            mutex: &'a Mutex<T>,
            wait_node: WaitNode,
        }

        unsafe impl<'a, T> Send for FutureLock<'a, T> {}

        impl<'a, T> Future for FutureLock<'a, T> {
            type Output = MutexGuard<'a, T>;

            fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
                if acquire(
                    &self.mutex.state,
                    &self.wait_node,
                    ctx.waker(),
                    self.wait_node.flags.get(),
                ) {
                    Poll::Ready(MutexGuard { mutex: self.mutex })
                } else {
                    Poll::Pending
                }
            }
        }

        FutureLock {
            mutex: self,
            wait_node: WaitNode::new(),
        }
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
    pub unsafe fn force_unlock(&self) {
        release_unfair(&self.state);
    }

    /// Forcibly unlocks the mutex using a fair unlock procotol,
    /// allowing a sleeping waker to acquire the queue if there is one
    /// instead of possibly re-acquiring it immediately ourselves in the future.
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
    pub unsafe fn force_unlock_fair(&self) {
        release_fair(&self.state);
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

unsafe impl<'a, T: Send> Send for MutexGuard<'a, T> {}
unsafe impl<'a, T: Sync> Sync for MutexGuard<'a, T> {}

impl<'a, T> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        release_unfair(&self.mutex.state);
    }
}

impl<'a, T> DerefMut for MutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.value.get() }
    }
}

impl<'a, T> Deref for MutexGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
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
        (&**self).fmt(f)
    }
}

impl<'a, T> MutexGuard<'a, T> {
    /// Makes a new `MappedMutexGuard` for a component of the locked data.
    ///
    /// This operation cannot fail as the `MutexGuard` passed
    /// in already locked the mutex.
    ///
    /// This is an associated function that needs to be
    /// used as `MutexGuard::map(...)`. A method would interfere with methods of
    /// the same name on the contents of the locked data.
    pub fn map<U>(this: Self, f: impl FnOnce(&mut T) -> &mut U) -> MappedMutexGuard<'a, U> {
        let state = &this.mutex.state;
        let value = f(unsafe { &mut *this.mutex.value.get() });
        mem::forget(this);
        MappedMutexGuard { state, value }
    }

    /// Attempts to make a new `MappedMutexGuard` for a component of the
    /// locked data. The original guard is returned if the closure returns `None`.
    ///
    /// This operation cannot fail as the `MutexGuard` passed
    /// in already locked the mutex.
    ///
    /// This is an associated function that needs to be
    /// used as `MutexGuard::try_map(...)`. A method would interfere with methods of
    /// the same name on the contents of the locked data.
    pub fn try_map<U>(
        this: Self,
        f: impl FnOnce(&mut T) -> Option<&mut U>,
    ) -> Result<MappedMutexGuard<'a, U>, Self> {
        match f(unsafe { &mut *this.mutex.value.get() }) {
            None => Err(this),
            Some(value) => {
                let state = &this.mutex.state;
                mem::forget(this);
                Ok(MappedMutexGuard { state, value })
            }
        }
    }

    /// Unlocks the mutex using a fair unlock protocol.
    ///
    /// By default, mutexes are unfair and allow the current future to re-lock
    /// the mutex before another has the chance to acquire the lock, even if
    /// that future has been blocked on the mutex for a long time. This can
    /// result in one future acquiring a mutex many more times than other futures.
    ///
    /// However in some cases it can be beneficial to ensure fairness by forcing
    /// the lock to pass on to a waiting thread if there is one. This is done by
    /// using this method instead of dropping the `MutexGuard` normally.
    #[inline]
    pub fn unlock_fair(self) {
        let state = &self.mutex.state;
        mem::forget(self);
        unsafe { release_fair(state) };
    }

    /// Temporarily yields the mutex to a waiting thread if there is one.
    ///
    /// This method is functionally equivalent to calling `unlock_fair` followed
    /// by `lock`, however it can be much more efficient in the case where there
    /// are no waiting threads.
    #[inline]
    pub fn bump(&'a mut self) -> impl Future<Output = ()> + 'a {
        // TODO: replace with more efficient version:
        // - if queue is empty: return
        // - if node in queue:
        //   - replace tail with ourselves
        //   - wake up old tail with handoff
        //   - try to acquire the lock ourselves
        self.unlocked_fair(|| {})
    }

    /// Temporarily unlocks the mutex to execute the given function.
    ///
    /// This is safe because `&mut` guarantees that there exist no other
    /// references to the data protected by the mutex.
    pub fn unlocked<R: 'a>(
        &'a mut self,
        f: impl FnOnce() -> R + 'a,
    ) -> impl Future<Output = R> + 'a {
        let state = &self.mutex.state;
        release_unfair(state);
        FutureUnlock {
            state,
            mutex: self,
            wait_node: WaitNode::new(),
            output: Cell::new(MaybeUninit::new(f())),
        }
    }

    /// Temporarily unlocks the mutex to execute the given function.
    ///
    /// The mutex is unlocked using a fair unlock protocol.
    ///
    /// This is safe because `&mut` guarantees that there exist no other
    /// references to the data protected by the mutex.
    pub fn unlocked_fair<R: 'a>(
        &'a mut self,
        f: impl FnOnce() -> R + 'a,
    ) -> impl Future<Output = R> + 'a {
        let state = &self.mutex.state;
        unsafe { release_fair(state) };
        FutureUnlock {
            state,
            mutex: self,
            wait_node: WaitNode::new(),
            output: Cell::new(MaybeUninit::new(f())),
        }
    }
}

/// An RAII mutex guard returned by `MutexGuard::map`, which can point to a
/// subfield of the protected data.
///
/// The main difference between `MappedMutexGuard` and `MutexGuard` is that the
/// former doesn't support temporarily unlocking and re-locking, since that
/// could introduce soundness issues if the locked object is modified by another
/// thread.
pub struct MappedMutexGuard<'a, T> {
    state: &'a AtomicUsize,
    value: *mut T,
}

unsafe impl<'a, T: Send> Send for MappedMutexGuard<'a, T> {}
unsafe impl<'a, T: Sync> Sync for MappedMutexGuard<'a, T> {}

impl<'a, T> Drop for MappedMutexGuard<'a, T> {
    fn drop(&mut self) {
        release_unfair(self.state);
    }
}

impl<'a, T> DerefMut for MappedMutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value }
    }
}

impl<'a, T> Deref for MappedMutexGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.value }
    }
}

impl<'a, T: fmt::Debug> fmt::Debug for MappedMutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<'a, T: fmt::Display> fmt::Display for MappedMutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (&**self).fmt(f)
    }
}

impl<'a, T: 'a> MappedMutexGuard<'a, T> {
    /// Makes a new `MappedMutexGuard` for a component of the locked data.
    ///
    /// This operation cannot fail as the `MappedMutexGuard` passed
    /// in already locked the mutex.
    ///
    /// This is an associated function that needs to be
    /// used as `MappedMutexGuard::map(...)`. A method would interfere with methods of
    /// the same name on the contents of the locked data.
    pub fn map<U>(this: Self, f: impl FnOnce(&mut T) -> &mut U) -> MappedMutexGuard<'a, U> {
        let state = this.state;
        let value = f(unsafe { &mut *this.value });
        mem::forget(this);
        MappedMutexGuard { state, value }
    }

    /// Attempts to make a new `MappedMutexGuard` for a component of the
    /// locked data. The original guard is returned if the closure returns `None`.
    ///
    /// This operation cannot fail as the `MappedMutexGuard` passed
    /// in already locked the mutex.
    ///
    /// This is an associated function that needs to be
    /// used as `MappedMutexGuard::try_map(...)`. A method would interfere with methods of
    /// the same name on the contents of the locked data.
    pub fn try_map<U>(
        this: Self,
        f: impl FnOnce(&mut T) -> Option<&mut U>,
    ) -> Result<MappedMutexGuard<'a, U>, Self> {
        match f(unsafe { &mut *this.value }) {
            None => Err(this),
            Some(value) => {
                let state = this.state;
                mem::forget(this);
                Ok(MappedMutexGuard { state, value })
            }
        }
    }

    /// Unlocks the mutex using a fair unlock protocol.
    ///
    /// By default, mutexes are unfair and allow the current future to re-lock
    /// the mutex before another has the chance to acquire the lock, even if
    /// that future has been blocked on the mutex for a long time. This can
    /// result in one future acquiring a mutex many more times than other futures.
    ///
    /// However in some cases it can be beneficial to ensure fairness by forcing
    /// the lock to pass on to a waiting thread if there is one. This is done by
    /// using this method instead of dropping the `MappedMutexGuard` normally.
    #[inline]
    pub fn unlock_fair(self) {
        let state = self.state;
        mem::forget(self);
        unsafe { release_fair(state) };
    }
}

/// Future used to re-acquire the mutex
/// from an `unlocked*` function
#[allow(dead_code)]
struct FutureUnlock<'a, T, M> {
    state: &'a AtomicUsize,
    mutex: &'a mut M,
    wait_node: WaitNode,
    output: Cell<MaybeUninit<T>>,
}

unsafe impl<'a, T, M> Send for FutureUnlock<'a, T, M> {}

impl<'a, T, M> Future for FutureUnlock<'a, T, M> {
    type Output = T;

    // After acquiring the mutex, return the output by consuming it.
    // Subsequent calls to poll after returning `Poll::Ready()` is UB
    // as it calls `MaybeUninit::assume_init()` on a `MaybeUninit::uninit()`.
    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        if acquire(
            self.state,
            &self.wait_node,
            ctx.waker(),
            self.wait_node.flags.get(),
        ) {
            Poll::Ready(unsafe {
                mem::replace(&mut *self.output.as_ptr(), MaybeUninit::uninit()).assume_init()
            })
        } else {
            Poll::Pending
        }
    }
}

/// Flag set on the [`WaitNode`] to
/// indicate that the futuer which woke
/// it up kept the mutex locked and that
/// they should assume to now have acquired it.
const WAIT_FLAG_HANDOFF: u8 = 1 << 0;

/// Flag set on the [`WaitNode`] to
/// indicate that there is an active
/// `Waker` inside the `waker` field.
const WAIT_FLAG_WAKER: u8 = 1 << 1;

struct WaitNode {
    flags: Cell<u8>,
    _pinned: PhantomPinned,
    prev: Cell<MaybeUninit<*const Self>>,
    next: Cell<MaybeUninit<*const Self>>,
    tail: Cell<MaybeUninit<*const Self>>,
    waker: Cell<MaybeUninit<Waker>>,
}

/// Make sure to drop the waker if there is one
impl Drop for WaitNode {
    fn drop(&mut self) {
        if self.flags.get() & WAIT_FLAG_WAKER != 0 {
            mem::drop(unsafe {
                mem::replace(&mut *self.waker.as_ptr(), MaybeUninit::uninit()).assume_init()
            })
        }
    }
}

impl WaitNode {
    /// Creates a new empty `WaitNode`.
    /// The fields use `MaybeUninit` to avoid
    /// writing to any stack/future allocated
    /// memory for the lock/unlock fast path.
    pub const fn new() -> Self {
        Self {
            flags: Cell::new(0),
            _pinned: PhantomPinned,
            prev: Cell::new(MaybeUninit::uninit()),
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
            waker: Cell::new(MaybeUninit::uninit()),
        }
    }

    /// Find the tail of the wait queue.
    ///
    /// Starting from the head of the queue as self,
    /// traverse the linked list until there is a node
    /// with the `tail` field set to non null, forming
    /// the doubly linked `prev` links in the process.
    /// The first node to be pushed to an empty queue
    /// sets the tail to point to itself.
    ///
    /// After finding the tail, store it in our `tail` field
    /// to cache it making subsequent calls for newer nodes
    /// added to resolve it faster than traversing again.
    pub unsafe fn find_tail(&self) -> &Self {
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
}

const MUTEX_LOCK: usize = 1 << 0;
const QUEUE_LOCK: usize = 1 << 1;
const QUEUE_MASK: usize = !(QUEUE_LOCK | MUTEX_LOCK);

/// Fast path for acquiring the mutex
#[inline]
fn try_acquire(state_ref: &AtomicUsize) -> bool {
    state_ref
        .compare_exchange_weak(0, MUTEX_LOCK, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

/// Test if the mutex was handed off to us before doing any atomic operations.
/// If not, go through the normal acquire root, potentially adding our wait_node
/// into the wait queue if the mutex is held and we're not already in the queue.
#[inline]
fn acquire(state_ref: &AtomicUsize, wait_node: &WaitNode, waker: &Waker, flags: u8) -> bool {
    (flags & WAIT_FLAG_HANDOFF != 0)
        || try_acquire(state_ref)
        || acquire_slow(state_ref, wait_node, waker, flags)
}

#[cold]
fn acquire_slow(
    state_ref: &AtomicUsize,
    wait_node: &WaitNode,
    waker: &Waker,
    mut flags: u8,
) -> bool {
    // check if we're already in the queue to not add ourselves again.
    let enqueued = flags & WAIT_FLAG_WAKER != 0;
    let mut state = state_ref.load(Ordering::Relaxed);
    loop {
        // try to acquire the mutex if its unlocked
        if state & MUTEX_LOCK == 0 {
            match state_ref.compare_exchange_weak(
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

        // if the mutex is locked, and were already in the queue, start pending.
        if enqueued {
            return false;
        }

        // Lazy initialize the WaitNode with the waker.
        // This both acts as an optimization for the MaybeUninit
        // and serves as a way to memoize the waker.clone() call
        // as it could be arbitrarily expensive.
        if flags & WAIT_FLAG_WAKER == 0 {
            flags |= WAIT_FLAG_WAKER;
            wait_node.flags.set(flags);
            wait_node.prev.set(MaybeUninit::new(null()));
            wait_node.waker.set(MaybeUninit::new(waker.clone()));
        }

        // prepare our node to be added as the new head of the wait queue.
        let head = (state & QUEUE_MASK) as *const WaitNode;
        wait_node.next.set(MaybeUninit::new(head));
        if head.is_null() {
            wait_node.tail.set(MaybeUninit::new(wait_node));
        } else {
            wait_node.tail.set(MaybeUninit::new(null()));
        }

        // try to push ourselves to the head of the wait queue
        assert!(align_of::<WaitNode>() > !QUEUE_MASK);
        match state_ref.compare_exchange_weak(
            state,
            (wait_node as *const _ as usize) | (state & !QUEUE_MASK),
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            Ok(_) => return false,
            Err(s) => state = s,
        }
    }
}

// Default fast path for normal unlocking.
#[inline]
fn release_unfair(state_ref: &AtomicUsize) {
    // Unlock the mutex immediately without looking at the queue.
    // fetch_sub(MUTEX) can be implemented more efficiently on
    // common platforms compared to fetch_and(!MUTEX_LOCK).
    let state = state_ref.fetch_sub(MUTEX_LOCK, Ordering::Release);

    // If the queue isn't locked and there are nodes waiting,
    // go try and lock the queue in order to pop a node off and wake it up.
    if (state & QUEUE_LOCK == 0) && (state & QUEUE_MASK != 0) {
        unsafe { release_unfair_slow(state_ref) };
    }
}

#[cold]
unsafe fn release_unfair_slow(state_ref: &AtomicUsize) {
    // In order to pop a node from the queue, we need to acquire the queue lock.
    let mut state = state_ref.load(Ordering::Relaxed);
    loop {
        if (state & QUEUE_LOCK != 0) || (state & QUEUE_MASK == 0) {
            return;
        }
        match state_ref.compare_exchange_weak(
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
        // get the tail node of the queue to pop it
        let head = &*((state & QUEUE_MASK) as *const WaitNode);
        let tail = head.find_tail();

        // if the mutex is locked, let the unlocker take care of waking up the node.
        if state & MUTEX_LOCK != 0 {
            match state_ref.compare_exchange_weak(
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

        // try to pop the tail from the queue, unlocking the queue at the same time.
        let new_tail = tail.prev.get().assume_init();
        if new_tail.is_null() {
            loop {
                match state_ref.compare_exchange_weak(
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
            state_ref.fetch_and(!QUEUE_LOCK, Ordering::Release);
        }

        // wake up the waker which lives on the tail node by consuming it.
        let waker = mem::replace(&mut *tail.waker.as_ptr(), MaybeUninit::uninit());
        tail.flags.set(tail.flags.get() & !WAIT_FLAG_WAKER);
        waker.assume_init().wake();
        return;
    }
}

unsafe fn release_fair(state_ref: &AtomicUsize) {
    let mut state = state_ref.load(Ordering::Acquire);
    loop {
        let head = (state & QUEUE_MASK) as *const WaitNode;

        // if the wait queue is empty, try to just unlock
        if head.is_null() {
            match state_ref.compare_exchange_weak(state, 0, Ordering::Release, Ordering::Relaxed) {
                Ok(_) => return,
                Err(s) => state = s,
            }
            fence(Ordering::Acquire);
            continue;
        }

        // prepare to pop the tail from the wait queue
        let head = &*head;
        let tail = head.find_tail();
        let new_tail = tail.prev.get().assume_init();

        // handle the case where the tail is the last node in the queue
        // by updating the state to zero out the queue mask without unlocking the mutex.
        if new_tail.is_null() {
            if let Err(s) = state_ref.compare_exchange_weak(
                state,
                MUTEX_LOCK,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                state = s;
                fence(Ordering::Acquire);
                continue;
            }
        } else {
            head.tail.set(MaybeUninit::new(new_tail));
        }

        // wake up the tail node using direct handoff without unlocking the mutex.
        // the woken up node will see the handoff flags when acquring and assume it holds the mutex.
        let waker = mem::replace(&mut *tail.waker.as_ptr(), MaybeUninit::uninit());
        tail.flags
            .set((tail.flags.get() & !WAIT_FLAG_WAKER) | WAIT_FLAG_HANDOFF);
        waker.assume_init().wake();
        return;
    }
}
