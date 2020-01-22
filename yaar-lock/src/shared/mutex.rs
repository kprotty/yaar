use core::{
    cell::Cell,
    marker::PhantomPinned,
    mem::{self, MaybeUninit},
    ptr::null,
    sync::atomic::{fence, spin_loop_hint, AtomicUsize, Ordering},
};

pub const WAIT_NODE_INIT: u8 = 1 << 0;
pub const WAIT_NODE_HANDOFF: u8 = 1 << 2;

pub struct WaitNode<Waker> {
    pub prev: Cell<MaybeUninit<*const Self>>,
    pub next: Cell<MaybeUninit<*const Self>>,
    pub tail: Cell<MaybeUninit<*const Self>>,
    pub waker: Cell<MaybeUninit<Waker>>,
    pub flags: Cell<u8>,
    _pinned: PhantomPinned,
}

impl<Waker> Default for WaitNode<Waker> {
    fn default() -> Self {
        Self {
            prev: Cell::new(MaybeUninit::uninit()),
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
            waker: Cell::new(MaybeUninit::uninit()),
            flags: Cell::new(0),
            _pinned: PhantomPinned,
        }
    }
}

impl<Waker> WaitNode<Waker> {
    fn find_tail<'a>(&self) -> &'a Self {
        unsafe {
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
}

const MUTEX_LOCK: usize = 1 << 0;
const QUEUE_LOCK: usize = 1 << 1;
const QUEUE_MASK: usize = !(MUTEX_LOCK | QUEUE_LOCK);

pub struct WordLock {
    state: AtomicUsize,
}

impl WordLock {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
        }
    }

    #[inline]
    pub fn try_lock(&self) -> bool {
        self.state
            .compare_exchange_weak(0, MUTEX_LOCK, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    #[inline]
    pub fn lock<F, Parker>(
        &self,
        max_spin_doubling: usize,
        wait_node: &WaitNode<Parker>,
        new_waker: F,
    ) -> bool
    where
        F: FnOnce() -> Parker,
    {
        let flags = wait_node.flags.get();
        (flags & WAIT_NODE_HANDOFF != 0)
            || self.try_lock()
            || self.lock_slow(
                flags,
                max_spin_doubling,
                wait_node,
                MaybeUninit::new(new_waker),
            )
    }

    #[cold]
    fn lock_slow<F, Parker>(
        &self,
        mut flags: u8,
        max_spin_doubling: usize,
        wait_node: &WaitNode<Parker>,
        mut new_waker: MaybeUninit<F>,
    ) -> bool
    where
        F: FnOnce() -> Parker,
    {
        let mut spin = 0;
        let is_waiting = flags & WAIT_NODE_INIT != 0;
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
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

            if is_waiting {
                return false;
            }

            let head = (state & QUEUE_MASK) as *const WaitNode<Parker>;
            if head.is_null() && spin < max_spin_doubling {
                spin += 1;
                (0..(1 << spin)).for_each(|_| spin_loop_hint());
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            if flags & WAIT_NODE_INIT == 0 {
                flags |= WAIT_NODE_INIT;
                wait_node.flags.set(flags);
                wait_node.prev.set(MaybeUninit::new(null()));
                wait_node.waker.set(MaybeUninit::new(unsafe {
                    (mem::replace(&mut new_waker, MaybeUninit::uninit()).assume_init())()
                }));
            }

            wait_node.next.set(MaybeUninit::new(head));
            if head.is_null() {
                wait_node.tail.set(MaybeUninit::new(wait_node));
            } else {
                wait_node.tail.set(MaybeUninit::new(null()));
            }

            debug_assert!(mem::align_of::<WaitNode<Parker>>() > !QUEUE_MASK);
            match self.state.compare_exchange_weak(
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
    pub fn unlock_unfair<'a, Waker>(&self) -> Option<&'a WaitNode<Waker>> {
        let state = self.state.fetch_sub(MUTEX_LOCK, Ordering::Release);
        if (state & QUEUE_MASK != 0) && (state & QUEUE_LOCK == 0) {
            self.unlock_unfair_slow()
        } else {
            None
        }
    }

    #[cold]
    fn unlock_unfair_slow<'a, Waker>(&self) -> Option<&'a WaitNode<Waker>> {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if (state & QUEUE_MASK == 0) || (state & QUEUE_LOCK != 0) {
                return None;
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
            let head = unsafe { &*((state & QUEUE_MASK) as *const WaitNode<Waker>) };
            let tail = head.find_tail();

            if state & MUTEX_LOCK != 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state & !QUEUE_LOCK,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return None,
                    Err(s) => state = s,
                }
                fence(Ordering::Acquire);
                continue;
            }

            let new_tail = unsafe { tail.prev.get().assume_init() };
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

            return Some(tail);
        }
    }

    pub fn unlock_fair<'a, Waker>(&self) -> Option<&'a WaitNode<Waker>> {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            let head = (state & QUEUE_MASK) as *const WaitNode<Waker>;
            if head.is_null() {
                match self.state.compare_exchange_weak(
                    state,
                    0,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return None,
                    Err(s) => state = s,
                }
                fence(Ordering::Acquire);
                continue;
            }

            let head = unsafe { &*head };
            let tail = head.find_tail();
            let new_tail = unsafe { tail.prev.get().assume_init() };

            if new_tail.is_null() {
                if let Err(s) = self.state.compare_exchange_weak(
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

            tail.flags.set(tail.flags.get() | WAIT_NODE_HANDOFF);
            return Some(tail);
        }
    }

    #[inline]
    pub fn bump<'a, F, Parker>(
        &self,
        wait_node: &WaitNode<Parker>,
        new_waker: F,
    ) -> Option<&'a WaitNode<Parker>>
    where
        F: FnOnce() -> Parker,
    {
        let state = self.state.load(Ordering::Acquire);
        let head = (state & QUEUE_MASK) as *const WaitNode<Parker>;
        if head.is_null() {
            return None;
        }

        let head = unsafe { &*head };
        let tail = head.find_tail();

        wait_node.flags.set(WAIT_NODE_INIT);
        wait_node.prev.set(tail.prev.get());
        wait_node.next.set(MaybeUninit::new(null()));
        wait_node.waker.set(MaybeUninit::new(new_waker()));
        wait_node.tail.set(MaybeUninit::new(wait_node));

        tail.flags.set(tail.flags.get() | WAIT_NODE_HANDOFF);
        Some(tail)
    }
}
