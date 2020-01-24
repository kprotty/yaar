use super::{ThreadEvent, WaitNode};
use core::{
    ptr::null,
    marker::PhantomData,
    sync::atomic::{fence, spin_loop_hint, Ordering, AtomicUsize},
};

#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type Mutex<T> = RawMutex<T, super::OsThreadEvent>;

#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type MutexGuard<'a, T> = RawMutexGuard<'a, T, super::OsThreadEvent>;

pub type RawMutex<T, Event> = lock_api::Mutex<WordLock<Event>, T>;

pub type RawMutexGuard<'a, T, Event> = lock_api::MutexGuard<'a, WordLock<Event>, T>;

#[doc(hidden)]
pub struct WordLock<Event: ThreadEvent> {
    state: AtomicUsize,
    phantom: PhantomData<Event>,
}

const MUTEX_LOCK: usize = 1 << 0;
const QUEUE_LOCK: usize = 1 << 1;
const QUEUE_MASK: usize = !(QUEUE_LOCK | MUTEX_LOCK);

unsafe impl<Event: ThreadEvent> lock_api::RawMutex for WordLock<Event> {
    const INIT: Self = Self {
        state: AtomicUsize::new(0),
        phantom: PhantomData,
    };

    type GuardMarker = lock_api::GuardSend;

    fn try_lock(&self) -> bool {
        self.state
            .compare_exchange_weak(0, MUTEX_LOCK, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    fn lock(&self) {
        if !self.try_lock() {
            let wait_node = WaitNode::<Event>::default();
            self.lock_slow(&wait_node);
        }
    }

    fn unlock(&self) {
        let state = self.state.fetch_sub(MUTEX_LOCK, Ordering::Release);
        if (state & QUEUE_LOCK == 0) && (state & QUEUE_MASK != 0) {
            self.unlock_slow();
        }
    }
}

impl<Event: ThreadEvent> WordLock<Event> {
    #[cold]
    fn lock_slow(&self, wait_node: &WaitNode<Event>) {
        const MAX_SPIN_DOUBLING: usize = 4;

        let mut spin = 0;
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state & MUTEX_LOCK == 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state | MUTEX_LOCK,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(s) => state = s,
                }
                continue;
            }

            let head = (state & QUEUE_MASK) as *const WaitNode<Event>;
            if head.is_null() && spin < MAX_SPIN_DOUBLING {
                spin += 1;
                (0..(1 << spin)).for_each(|_| spin_loop_hint());
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            wait_node.init(head);
            if let Err(s) = self.state.compare_exchange_weak(
                state,
                (&wait_node as *const _ as usize) | (state & !QUEUE_MASK),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                state = s;
                continue;
            }

            if wait_node.wait() {
                return;
            } else {
                spin = 0;
                wait_node.reset();
                state = self.state.load(Ordering::Relaxed);
            }
        }
    }

    #[cold]
    fn unlock_slow(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if (state & QUEUE_LOCK != 0) || (state & QUEUE_MASK == 0) {
                return;
            } else {
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
        }

        'outer: loop {
            let (head, tail, new_tail) = unsafe {
                let head = &*((state & QUEUE_MASK) as *const WaitNode<Event>);
                let tail = &*head.get_tail();
                (head, tail, tail.get_prev())
            };

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
                head.set_tail(new_tail);
                self.state.fetch_and(!QUEUE_LOCK, Ordering::Release);
            }

            tail.notify(false);
            return;
        }
    }
}

unsafe impl<Event: ThreadEvent> lock_api::RawMutexFair for WordLock<Event> {
    fn unlock_fair(&self) {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            let head = (state & QUEUE_MASK) as *const WaitNode<Event>;

            if head.is_null() {
                match self.state.compare_exchange_weak(
                    state,
                    0,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(s) => state = s
                }
                fence(Ordering::Acquire);
                continue;
            }

            let (head, tail, new_tail) = unsafe {
                let head = &*head;
                let tail = &*head.get_tail();
                (head, tail, tail.get_prev())
            };

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
                head.set_tail(new_tail);
            }

            tail.notify(true);
            return;
        }
    }

    fn bump(&self) {
        let state = self.state.load(Ordering::Acquire);
        let head = (state & QUEUE_MASK) as *const WaitNode<Event>;
        if head.is_null() {
            return;
        }

        let (head, tail, prev) = unsafe {
            let head = &*head;
            let tail = &*head.get_tail();
            (head, tail, tail.get_prev())
        };

        let wait_node = WaitNode::<Event>::default();
        wait_node.init(null());
        wait_node.set_prev(prev);
        head.set_tail(&wait_node);

        tail.notify(true);

        if wait_node.wait() {
            return;
        } else {
            wait_node.reset();
            self.lock_slow(&wait_node);
        }
    }
}
