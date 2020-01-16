use core::{
    pin::Pin,
    future::Future,
    ptr::null,
    mem::{self, align_of, MaybeUninit},
    cell::{Cell, UnsafeCell},
    task::{Poll, Waker, Context},
    sync::atomic::{fence, Ordering, AtomicUsize},
};

pub struct Mutex<T> {
    value: UnsafeCell<T>,
    state: AtomicUsize,
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            value: UnsafeCell::new(value),
            state: AtomicUsize::new(0),
        }
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    pub fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value.get() }
    }

    #[inline]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        if try_acquire(&self.state) {
            Some(MutexGuard { mutex: self })
        } else {
            None
        }
    }

    pub fn lock(&self) -> impl Future<Output = MutexGuard<'_, T>> + Send {
        struct FutureLock<'a, T> {
            mutex: &'a Mutex<T>,
            wait_node: WaitNode,
        }

        unsafe impl<'a, T> Send for FutureLock<'a, T> {}

        impl<'a, T> Future for FutureLock<'a, T> {
            type Output = MutexGuard<'a, T>;

            fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
                if acquire(&self.mutex.state, &self.wait_node, ctx.waker(), self.wait_node.flags.get()) {
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
}

pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
}

impl<'a, T> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        release_unfair(&self.mutex.state);
    }
}

impl<'a, T> MutexGuard<'a, T> {
    pub fn map<U>(self, f: impl FnOnce(&mut T) -> &mut U) -> MappedMutexGuard<'a, U> {
        let state = &self.mutex.state;
        let value = f(unsafe { &mut *self.mutex.value.get() });
        mem::forget(self);
        MappedMutexGuard { state, value }
    }

    pub fn try_map<U>(self, f: impl FnOnce(&mut T) -> Option<&mut U>) -> Result<MappedMutexGuard<'a, U>, Self> {
        match f(unsafe { &mut *self.mutex.value.get() }) {
            None => Err(self),
            Some(value) => {
                let state = &self.mutex.state;
                mem::forget(self);
                Ok(MappedMutexGuard { state, value })
            }
        }
    }

    #[inline]
    pub fn unlock_fair(self) {
        let state = &self.mutex.state;
        mem::forget(self);
        unsafe { release_fair(state) };
    }

    #[inline]
    pub fn bump(&mut self) -> impl Future<Output = ()> + 'a {
        self.unlocked_fair(|| {})
    }

    pub fn unlocked_fair<R: 'a>(&mut self, f: impl FnOnce() -> R + 'a) -> impl Future<Output = R> + 'a {
        unsafe { release_fair(&self.mutex.state) };
        FutureUnlock {
            state: &self.mutex.state,
            wait_node: WaitNode::new(),
            output: Cell::new(MaybeUninit::new(f())),
        }
    }
}

pub struct MappedMutexGuard<'a, T> {
    state: &'a AtomicUsize,
    value: *mut T,
}

impl<'a, T> Drop for MappedMutexGuard<'a, T> {
    fn drop(&mut self) {
        release_unfair(self.state);
    }
}

impl<'a, T: 'a> MappedMutexGuard<'a, T> {
    pub fn map<U>(self, f: impl FnOnce(&mut T) -> &mut U) -> MappedMutexGuard<'a, U> {
        let state = self.state;
        let value = f(unsafe { &mut *self.value });
        mem::forget(self);
        MappedMutexGuard { state, value }
    }

    pub fn try_map<U>(self, f: impl FnOnce(&mut T) -> Option<&mut U>) -> Result<MappedMutexGuard<'a, U>, Self> {
        match f(unsafe { &mut *self.value }) {
            None => Err(self),
            Some(value) => {
                let state = self.state;
                mem::forget(self);
                Ok(MappedMutexGuard { state, value })
            }
        }
    }

    #[inline]
    pub fn unlock_fair(self) {
        let state = self.state;
        mem::forget(self);
        unsafe { release_fair(state) };
    }

    #[inline]
    pub fn bump(&mut self) -> impl Future<Output = ()> + 'a {
        self.unlocked_fair(|| {})
    }

    pub fn unlocked_fair<R: 'a>(&mut self, f: impl FnOnce() -> R + 'a) -> impl Future<Output = R> + 'a {
        unsafe { release_fair(self.state) };
        FutureUnlock {
            state: self.state,
            wait_node: WaitNode::new(),
            output: Cell::new(MaybeUninit::new(f())),
        }
    }
}

struct FutureUnlock<'a, T> {
    state: &'a AtomicUsize,
    wait_node: WaitNode,
    output: Cell<MaybeUninit<T>>,
}

unsafe impl<'a, T> Send for FutureUnlock<'a, T> {}

impl<'a, T> Future for FutureUnlock<'a, T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        if acquire(self.state, &self.wait_node, ctx.waker(), self.wait_node.flags.get()) {
            Poll::Ready(unsafe {
                mem::replace(&mut *self.output.as_ptr(), MaybeUninit::uninit()).assume_init()
            })
        } else {
            Poll::Pending
        }
    }
}

const WAIT_FLAG_HANDOFF: u8 = 1 << 0;
const WAIT_FLAG_WAKER: u8 = 1 << 1;

struct WaitNode {
    flags: Cell<u8>,
    prev: Cell<MaybeUninit<*const Self>>,
    next: Cell<MaybeUninit<*const Self>>,
    tail: Cell<MaybeUninit<*const Self>>,
    waker: Cell<MaybeUninit<Waker>>,
}

impl Drop for WaitNode {
    fn drop(&mut self) {
        if self.flags.get() & WAIT_FLAG_WAKER != 0 {
            mem::drop(unsafe {
                mem::replace(&mut *self.waker.as_ptr(), MaybeUninit::uninit())
                    .assume_init()
            })
        }
    }
}

impl WaitNode {
    pub const fn new() -> Self {
        Self {
            flags: Cell::new(0),
            prev: Cell::new(MaybeUninit::uninit()),
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
            waker: Cell::new(MaybeUninit::uninit()),
        }
    }

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

#[inline]
fn try_acquire(state_ref: &AtomicUsize) -> bool {
    state_ref
        .compare_exchange_weak(0, MUTEX_LOCK, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

#[inline]
fn acquire(state_ref: &AtomicUsize, wait_node: &WaitNode, waker: &Waker, flags: u8) -> bool {
    (flags & WAIT_FLAG_HANDOFF != 0) 
        || try_acquire(state_ref)
        || acquire_slow(state_ref, wait_node, waker, flags)
}

#[cold]
fn acquire_slow(state_ref: &AtomicUsize, wait_node: &WaitNode, waker: &Waker, mut flags: u8) -> bool {
    let enqueued = flags & WAIT_FLAG_WAKER != 0;
    let mut state = state_ref.load(Ordering::Relaxed);
    loop {
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
        
        if enqueued {
            return false;
        }

        if flags & WAIT_FLAG_WAKER == 0 {
            flags |= WAIT_FLAG_WAKER;
            wait_node.flags.set(flags);
            wait_node.prev.set(MaybeUninit::new(null()));
            wait_node.waker.set(MaybeUninit::new(waker.clone()));
        }
        
        let head = (state & QUEUE_MASK) as *const WaitNode;
        wait_node.next.set(MaybeUninit::new(head));
        if head.is_null() {
            wait_node.tail.set(MaybeUninit::new(wait_node));
        } else {
            wait_node.tail.set(MaybeUninit::new(null()));
        }

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

#[inline]
fn release_unfair(state_ref: &AtomicUsize) {
    let state = state_ref.fetch_sub(MUTEX_LOCK, Ordering::Release);
    if (state & QUEUE_LOCK == 0) && (state & QUEUE_MASK != 0) {
        unsafe { release_unfair_slow(state_ref) };
    }
}

#[cold]
unsafe fn release_unfair_slow(state_ref: &AtomicUsize) {
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
        let head = &*((state & QUEUE_MASK) as *const WaitNode);
        let tail = head.find_tail();

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

        if head.is_null() {
            match state_ref.compare_exchange_weak(
                state,
                0,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(s) => state = s,
            }
            fence(Ordering::Acquire);
            continue;
        }

        let head = &*head;
        let tail = head.find_tail();
        let new_tail = tail.prev.get().assume_init();
        
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
        }
        
        head.tail.set(MaybeUninit::new(new_tail));
        let waker = mem::replace(&mut *tail.waker.as_ptr(), MaybeUninit::uninit());
        tail.flags.set((tail.flags.get() & !WAIT_FLAG_WAKER) | WAIT_FLAG_HANDOFF);
        waker.assume_init().wake();
        return;
    }
}
