use super::{WaitNode, WAIT_NODE_INIT};
use core::{
    mem::MaybeUninit,
    ptr::null,
    sync::atomic::{AtomicUsize, Ordering},
};

const IS_SET: usize = 1;

pub struct WordEvent {
    state: AtomicUsize,
}

impl WordEvent {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
        }
    }

    #[inline]
    pub fn reset(&self) {
        self.state.store(0, Ordering::Relaxed);
    }

    #[inline]
    pub fn is_set(&self) -> bool {
        self.state.load(Ordering::Acquire) == IS_SET
    }

    #[inline]
    pub fn set<Waker, F>(&self, wake: F)
    where
        F: Fn(&WaitNode<Waker>),
    {
        let state = self.state.swap(IS_SET, Ordering::Release);
        let head = (state & !IS_SET) as *const WaitNode<Waker>;

        if !head.is_null() {
            unsafe {
                let mut node = (&*head).find_tail();
                loop {
                    wake(node);
                    let prev = node.prev.get().assume_init();
                    if prev.is_null() {
                        break;
                    } else {
                        node = &*prev;
                    }
                }
            }
        }
    }

    #[inline]
    pub fn wait<F, Waker>(&self, wait_node: &WaitNode<Waker>, new_waker: F) -> bool
    where
        F: FnOnce() -> Waker,
    {
        let state = self.state.load(Ordering::Acquire);
        if state != IS_SET {
            self.wait_slow(wait_node, new_waker)
        } else {
            false
        }
    }

    #[cold]
    fn wait_slow<F, Waker>(&self, wait_node: &WaitNode<Waker>, new_waker: F) -> bool
    where
        F: FnOnce() -> Waker,
    {
        wait_node.flags.set(WAIT_NODE_INIT);
        wait_node.prev.set(MaybeUninit::new(null()));
        wait_node.waker.set(MaybeUninit::new(new_waker()));

        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state == IS_SET {
                return false;
            }

            let head = (state & !IS_SET) as *const WaitNode<Waker>;
            wait_node.next.set(MaybeUninit::new(head));
            if head.is_null() {
                wait_node.tail.set(MaybeUninit::new(wait_node));
            } else {
                wait_node.tail.set(MaybeUninit::new(null()));
            }

            match self.state.compare_exchange_weak(
                state,
                wait_node as *const _ as usize,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(s) => state = s,
            }
        }
    }
}
