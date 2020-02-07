use super::WaitNode;
use crate::ThreadEvent;
use core::{
    marker::PhantomData,
    sync::atomic::{spin_loop_hint, Ordering, AtomicUsize},
};

const QUEUE_LOCK: usize = 1 << 0;
const QUEUE_MASK: usize = !(QUEUE_LOCK);

const DIVIDER: usize = (!0usize).count_ones() / 2;
const READER: usize = 1 << (DIVIDER - DIVIDER);
const WRITER: usize = 1 << DIVIDER;

fn get_writers(x: usize) -> usize {
    x >> DIVIDIER
}

fn get_readers(x: usize) -> usize {
    x & (WRITER - 1)
}

pub struct CoreRwLock<E> {
    state: AtomicUsize,
    queue: AtomicUsize,
    phantom: PhantomData<E>,
}

unsafe impl<E> Send for CoreRwLock<E> {}
unsafe impl<E: Sync> Sync for CoreRwLock<E> {}

unsafe impl<E: ThreadEvent> lock_api::RawRwLock for CoreRwLock<E> {
    const INIT: Self = Self {
        state: AtomicUsize::new(0),
        queue: AtomicUsize::new(0),
        phantom: PhantomData,
    };

    type GuardMarker = lock_api::GuardSend;

    fn try_lock_shared(&self) -> bool {
        let state = self.state.load(Ordering::Relaxed);
        if get_writers(state) != 0 {
            false
        } else if let Some(new_state) = state.checked_add(READER) {
            self.state
                .compare_exchange_weak(state, new_state, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
        } else {
            false
        }
    }

    fn lock_shared(&self) {
        if !self.try_lock_shared() {
            self.lock_shared_slow();
        } 
    }

    fn unlock_shared(&self) {
        let state = self.state.fetch_sub(READER, Ordering::Release);
        debug_assert_ne!(get_readers(state), 0);
        if (get_readers(state) == 1) && (get_writers(state) != 0) {
            self.unlock_shared_slow();
        }
    }

    fn try_lock_exclusive(&self) -> bool {

    }

    fn lock_exclusive(&self) {

    }

    fn unlock_exclusive(&self) {

    }
}

const MAX_SPIN_DOUBLING: usize = 4;


impl<E: ThreadEvent> CoreRwLock<E> {
    #[cold]
    fn lock_shared_slow(&self) {
        let mut spin = 0;
        let node = WaitNode::<E>::default();
        let mut state = self.state.load(Ordering::Relaxed);

        loop {
            if get_writers(state) == 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state.checked_add(READER)
                        .expect("RwLock reader count overflow"),
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(s) => state = s,
                }
                continue;
            }
            
            let head = self.queue.load(Ordering::Relaxed);
            let head = (head & QUEUE_MASK) as *const RwWaitNode<E>;
            if head.is_null() && spin < MAX_SPIN_DOUBLING {
                spin += 1;
                (0..(1 << spin)).for_each(|_| spin_loop_hint());
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            let new_head = node.enqueue(head);
            if self.queue.compare_exchange_weak(
                head,
                (new_head as usize) | (head & !QUEUE_MASK),
                Ordering::Release,
                Ordering::Relaxed,
            ).is_err() {
                state = self.state.load(Ordering::Relaxed);
                continue;
            }


        }
    }

    #[cold]
    fn unlock_shared_slow(&self) {
        let 
    }
}