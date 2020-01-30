use super::WaitNode;
use crate::ThreadEvent;
use core::{
    fmt,
    marker::PhantomData,
    sync::atomic::{fence, AtomicUsize, Ordering},
};

#[cfg(feature = "os")]
pub use self::if_os::*;
#[cfg(feature = "os")]
mod if_os {
    use super::*;
    use crate::OsThreadEvent;

    /// A [`WordEvent`] backed by [`OsThreadEvent`] for thread blocking.
    #[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
    pub type ResetEvent = WordEvent<OsThreadEvent>;
}

const IS_SET: usize = 0b1;

/// A word (`usize`) sized [`ThreadEvent`] implementation.
pub struct WordEvent<E> {
    state: AtomicUsize,
    phantom: PhantomData<E>,
}

unsafe impl<E: Send> Send for WordEvent<E> {}

unsafe impl<E: Sync> Sync for WordEvent<E> {}

impl<E> Default for WordEvent<E> {
    fn default() -> Self {
        Self::new(false)
    }
}

impl<E> WordEvent<E> {
    #[inline]
    pub fn new(is_set: bool) -> Self {
        Self {
            state: AtomicUsize::new(if is_set { IS_SET } else { 0 }),
            phantom: PhantomData,
        }
    }
}

impl<E: ThreadEvent> fmt::Debug for WordEvent<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WordEvent")
            .field("is_set", &self.is_set())
            .finish()
    }
}

impl<E: ThreadEvent> ThreadEvent for WordEvent<E> {
    #[inline]
    fn is_set(&self) -> bool {
        self.state.load(Ordering::Acquire) == IS_SET
    }

    #[inline]
    fn reset(&self) {
        self.state.store(0, Ordering::Relaxed);
    }

    #[inline]
    fn set(&self) {
        let state = self.state.swap(IS_SET, Ordering::Release);
        let head = (state & !IS_SET) as *const WaitNode<E>;
        if !head.is_null() {
            self.wake_slow(unsafe { &*head });
        }
    }

    #[inline]
    fn wait(&self) {
        if !self.is_set() {
            self.wait_slow();
        }
    }
}

impl<E: ThreadEvent> WordEvent<E> {
    #[cold]
    fn wake_slow(&self, head: &WaitNode<E>) {
        loop {
            let (new_tail, tail) = head.dequeue();
            tail.notify(false);
            if new_tail.is_null() {
                break;
            }
        }
    }

    #[cold]
    fn wait_slow(&self) {
        let wait_node = WaitNode::<E>::default();
        let mut state = self.state.load(Ordering::Acquire);

        loop {
            if state == IS_SET {
                return;
            }

            let head = (state & !IS_SET) as *const WaitNode<E>;
            if let Err(s) = self.state.compare_exchange_weak(
                state,
                wait_node.enqueue(head) as usize,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                fence(Ordering::Acquire);
                state = s;
                continue;
            }

            let _ = wait_node.wait();
            wait_node.reset();
            state = self.state.load(Ordering::Acquire);
            continue;
        }
    }
}

#[cfg(test)]
#[test]
fn test_reset_event() {
    use std::{cell::Cell, sync::Arc, thread};

    let event = ResetEvent::default();
    assert_eq!(event.is_set(), false);

    event.set();
    assert_eq!(event.is_set(), true);

    event.reset();
    assert_eq!(event.is_set(), false);

    struct Context {
        value: Cell<u128>,
        input: ResetEvent,
        output: ResetEvent,
    }

    unsafe impl Sync for Context {}

    let context = Arc::new(Context {
        value: Cell::new(0),
        input: ResetEvent::default(),
        output: ResetEvent::default(),
    });

    let receiver = {
        let context = context.clone();
        thread::spawn(move || {
            // wait for sender to update value and signal input
            context.input.wait();
            assert_eq!(context.value.get(), 1);

            // update value and signal output
            context.input.reset();
            context.value.set(2);
            context.output.set();

            // wait for sender to update value and signal final input
            context.input.wait();
            assert_eq!(context.value.get(), 3);
        })
    };

    let sender = move || {
        // update value and signal input
        assert_eq!(context.value.get(), 0);
        context.value.set(1);
        context.input.set();

        // wait for receiver to update value and signal output
        context.output.wait();
        assert_eq!(context.value.get(), 2);

        // update value and signal final input
        context.value.set(3);
        context.input.set();
    };

    sender();
    receiver.join().unwrap();
}
