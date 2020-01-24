use super::{WaitNode, WAIT_NODE_ACQUIRE, WAIT_NODE_INIT};
use core::{
    cell::Cell,
    mem::{align_of, MaybeUninit},
    ptr::null,
    sync::atomic::{fence, spin_loop_hint, AtomicUsize, Ordering},
};

const MUTEX_LOCK: usize = 1 << 0;
const QUEUE_LOCK: usize = 1 << 1;
const QUEUE_MASK: usize = !(MUTEX_LOCK | QUEUE_LOCK);

/// Adapted [`WordLock`] algorithm from parking_lot.
///
/// [`WordLock`]: https://github.com/Amanieu/parking_lot/blob/master/core/src/word_lock.rs
pub struct WordLock {
    state: AtomicUsize,
}

impl WordLock {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
        }
    }

    /// Fast-path acquire of the lock.
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
        (flags & WAIT_NODE_ACQUIRE != 0)
            || self.try_lock()
            || self.lock_slow(
                flags,
                max_spin_doubling,
                wait_node,
                Cell::new(MaybeUninit::new(new_waker)),
            )
    }

    /// Slow path for mutex locking.
    ///
    /// Returns true if acquiring the mutex succeeded.
    /// Returns false if it failed to acquire the mutex
    /// and the wait_node is in the wait queue.
    #[cold]
    fn lock_slow<F, Parker>(
        &self,
        mut flags: u8,
        max_spin_doubling: usize,
        wait_node: &WaitNode<Parker>,
        mut new_waker: Cell<MaybeUninit<F>>,
    ) -> bool
    where
        F: FnOnce() -> Parker,
    {
        // spin on the state, trying to either acquire the lock
        // or enqueue ourselves into the waiting queue.
        let mut spin = 0;
        let is_waiting = flags & WAIT_NODE_INIT != 0;
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            // try to acquire the lock if its unlocked
            if state & MUTEX_LOCK == 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state | MUTEX_LOCK,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Err(s) => state = s,
                    Ok(_) => {
                        wait_node.flags.set(flags | WAIT_NODE_ACQUIRE);
                        return true;
                    }
                }
                continue;
            }

            // should park since its already in the queue (for futures).
            if is_waiting {
                return false;
            }

            // try to spin checking for the lock
            // if theres no wait queue and if we haven't spun too much.
            let head = (state & QUEUE_MASK) as *const WaitNode<Parker>;
            if head.is_null() && spin < max_spin_doubling {
                spin += 1;
                (0..(1 << spin)).for_each(|_| spin_loop_hint());
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            // lazy initialize the wait node.
            // this is to memoize the waker as it
            // may be an owned resource (core::task::Waker)
            // or be relatively expensive to create (crate::sync::ThreadParker).
            if flags & WAIT_NODE_INIT == 0 {
                flags |= WAIT_NODE_INIT;
                wait_node.flags.set(flags);
                wait_node.prev.set(MaybeUninit::new(null()));
                wait_node.waker.set(MaybeUninit::new(unsafe {
                    (new_waker.replace(MaybeUninit::uninit()).assume_init())()
                }));
            }

            // prepare the node to be added to the queue.
            wait_node.next.set(MaybeUninit::new(head));
            if head.is_null() {
                wait_node.tail.set(MaybeUninit::new(wait_node));
            } else {
                wait_node.tail.set(MaybeUninit::new(null()));
            }

            // Try to add the node to the wait queue.
            debug_assert!(align_of::<WaitNode<Parker>>() > !QUEUE_MASK);
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
    fn lock_queue(&self, needs_node: bool) -> Result<usize, usize> {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            // the queue is already locked
            if state & QUEUE_LOCK != 0 {
                return Err(state);
            }

            // there are no nodes to dequeue
            if needs_node && (state & QUEUE_MASK == 0) {
                return Err(state);
            }

            // Use an acquire barrier when grabbing the QUEUE_LOCK,
            // since the state will be dereferenced as a WaitNode.
            match self.state.compare_exchange_weak(
                state,
                state | QUEUE_LOCK,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(s) => return Ok(s),
                Err(s) => state = s,
            }
        }
    }

    /// Fast-path for unlocking the mutex.
    /// Returns a wait node that was dequeued and should be woken up if any.
    #[inline]
    pub fn unlock_unfair<'a, Waker>(&self) -> Option<&'a WaitNode<Waker>> {
        // Use fetch_sub(1) to release the lock instead of fetch_and(!1)
        // as the former is implemented more efficiently on common platforms.
        // (e.g. for x86: `lock xadd` vs `lock cmpxchg` loop)
        let state = self.state.fetch_sub(MUTEX_LOCK, Ordering::Release);
        if (state & QUEUE_LOCK == 0) && (state & QUEUE_MASK != 0) {
            self.unlock_unfair_slow()
        } else {
            None
        }
    }

    /// Slow path for unlocking the mutex.
    /// Returns a wait node that was dequeued and should be woken up if any.
    #[cold]
    fn unlock_unfair_slow<'a, Waker>(&self) -> Option<&'a WaitNode<Waker>> {
        self.lock_queue(true).ok().and_then(|mut state| {
            // When re-iterating, need an Acquire barrier to observe
            // the node updates to the head Release'd from other threads.
            'outer: loop {
                let head = unsafe { &*((state & QUEUE_MASK) as *const WaitNode<Waker>) };
                let tail = head.find_tail();

                // if the mutex is currently locked,
                // let the lock holder take care of dequeuing & waking a node.
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

                // pop the tail from the wait queue
                let new_tail = unsafe { tail.prev.get().assume_init() };
                if new_tail.is_null() {
                    loop {
                        // zero out the queue node + unlock the queue if next is null
                        match self.state.compare_exchange_weak(
                            state,
                            state & MUTEX_LOCK,
                            Ordering::Release,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => break,
                            Err(s) => state = s,
                        }

                        // a new node was added to the queue, reprocess from the head.
                        if state & QUEUE_MASK != 0 {
                            fence(Ordering::Acquire);
                            continue 'outer;
                        }
                    }
                } else {
                    head.tail.set(MaybeUninit::new(new_tail));
                    self.state.fetch_and(!QUEUE_LOCK, Ordering::Release);
                }

                // return the dequeued tail to then wake.
                return Some(tail);
            }
        })
    }

    /// Similar to unlock, but directly hands off the lock
    /// to a waiting thread if any to prevent the current thread
    /// from re-acquring the lock multiple times if it could have.
    ///
    /// Returns a wait node that was dequeued and should be woken up if any.
    pub fn unlock_fair<'a, Waker>(&self) -> Option<&'a WaitNode<Waker>> {
        loop {
            match self.lock_queue(false) {
                Err(state) => match self.state.compare_exchange_weak(
                    state,
                    state & !QUEUE_LOCK,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return None,
                    Err(_) => spin_loop_hint(),
                },
                Ok(mut state) => loop {
                    // if the last node, consume it while releasing
                    // the mutex & queue locks at the same time.
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

                    // consume the tail node while keeping
                    // the mutex locked for a direct handoff.
                    if new_tail.is_null() {
                        if let Err(s) = self.state.compare_exchange_weak(
                            state,
                            MUTEX_LOCK,
                            Ordering::Release,
                            Ordering::Relaxed,
                        ) {
                            fence(Ordering::Acquire);
                            state = s;
                            continue;
                        }
                    } else {
                        head.tail.set(MaybeUninit::new(new_tail));
                        self.state.fetch_and(!QUEUE_LOCK, Ordering::Release);
                    }

                    // dont unlock the mutex, but instead transfer lock
                    // ownership to the queue node we just dequeued.
                    tail.flags.set(tail.flags.get() | WAIT_NODE_ACQUIRE);
                    return Some(tail);
                },
            }
        }
    }

    pub fn bump<'a, F, Parker>(
        &self,
        wait_node: &WaitNode<Parker>,
        new_waker: F,
    ) -> Option<&'a WaitNode<Parker>>
    where
        F: FnOnce() -> Parker,
    {
        self.lock_queue(true).ok().map(|state| {
            let head = unsafe { &*((state & QUEUE_MASK) as *const WaitNode<Parker>) };
            let tail = head.find_tail();

            // replace the tail of the queue with our wait_node
            wait_node.flags.set(WAIT_NODE_INIT);
            wait_node.prev.set(tail.prev.get());
            wait_node.next.set(MaybeUninit::new(null()));
            wait_node.waker.set(MaybeUninit::new(new_waker()));
            wait_node.tail.set(MaybeUninit::new(wait_node));
            head.tail.set(MaybeUninit::new(wait_node));

            // directly handoff the mutex lock to the old tail, now dequeued.
            tail.flags.set(tail.flags.get() | WAIT_NODE_ACQUIRE);
            tail
        })
    }

    /// Remove a waiting node from the queue for lock cancellation.
    pub fn remove_waiter<'a, Waker>(
        &self,
        wait_node: &WaitNode<Waker>,
    ) -> Option<&'a WaitNode<Waker>> {
        debug_assert!(wait_node.flags.get() & WAIT_NODE_INIT != 0);

        // Since the queue lock needs to be held, and future synchronization
        // primitives dont assume an OS, the best we can do is spinlock to
        // acquire it. So for now, lock cancellation is relatively expensive.
        loop {
            match self.lock_queue(true) {
                Err(_) => spin_loop_hint(),
                // When removing ourselves from the queue, we have to also wake up
                // a waiting node since the unlock*() functions above may bail out
                // with the assuming that another unlock*() function is doing the wakeup.
                Ok(mut state) => loop {
                    unsafe {
                        let head = &*((state & QUEUE_MASK) as *const WaitNode<Waker>);
                        let tail = head.find_tail();

                        let (tail, new_tail) = if (tail as *const _) == (wait_node as *const _) {
                            // handle the case of [new_tail, tail, wait_node]
                            let prev = wait_node.prev.get().assume_init();
                            if prev.is_null() {
                                (None, null())
                            } else {
                                let tail = &*prev;
                                let prev = tail.prev.get().assume_init();
                                if !prev.is_null() {
                                    (&*prev).next.set(MaybeUninit::new(null()));
                                }
                                (Some(tail), prev)
                            }
                        } else {
                            // handle the case of [?, wait_node, ?]
                            let prev = wait_node.prev.get().assume_init();
                            let next = wait_node.next.get().assume_init();
                            if !prev.is_null() {
                                (&*prev).next.set(MaybeUninit::new(next));
                            }
                            if !next.is_null() {
                                (&*next).prev.set(MaybeUninit::new(prev));
                            }

                            // handle the case of [new_tail, wait_node, tail]
                            let mut new_tail = tail.prev.get().assume_init();
                            if new_tail == (wait_node as *const _) {
                                new_tail = prev;
                                if !prev.is_null() {
                                    (&*prev).next.set(MaybeUninit::new(null()));
                                }
                            }
                            (Some(tail), new_tail)
                        };

                        // Pop the tail + dequeue ourselves + unlock the queue
                        if new_tail.is_null() {
                            if let Err(s) = self.state.compare_exchange_weak(
                                state,
                                state & MUTEX_LOCK,
                                Ordering::Release,
                                Ordering::Relaxed,
                            ) {
                                fence(Ordering::Acquire);
                                state = s;
                                continue;
                            }
                        } else {
                            head.tail.set(MaybeUninit::new(new_tail));
                            self.state.fetch_and(!QUEUE_LOCK, Ordering::Release);
                        }

                        return tail;
                    }
                },
            }
        }
    }
}
