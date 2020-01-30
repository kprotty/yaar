use super::{WaitNode, WaitNodeState};
use core::{
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use futures_core::future::FusedFuture;
use lock_api::{Mutex, RawMutex};

const IS_SET: usize = 0b1;

#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type ResetEvent = RawResetEvent<crate::sync::WordLock<crate::OsThreadEvent>>;

#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type WaitForEventFuture<'a> =
    RawWaitForEventFuture<'a, crate::sync::WordLock<crate::OsThreadEvent>>;

/// A future-aware manual reset event.
pub struct RawResetEvent<Raw: RawMutex> {
    state: Mutex<Raw, usize>,
}

unsafe impl<Raw: RawMutex + Send> Send for RawResetEvent<Raw> {}
unsafe impl<Raw: RawMutex + Sync> Sync for RawResetEvent<Raw> {}

impl<Raw: RawMutex> fmt::Debug for RawResetEvent<Raw> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResetEvent")
            .field("is_set", &self.is_set())
            .finish()
    }
}

impl<Raw: RawMutex> RawResetEvent<Raw> {
    /// Creates a new ResetEvent in the given state
    pub fn new(is_set: bool) -> Self {
        Self {
            state: Mutex::new(if is_set { IS_SET } else { 0 }),
        }
    }

    /// Resets the event.
    pub fn reset(&self) {
        *self.state.lock() = 0;
    }

    /// Returns whether the event is set
    pub fn is_set(&self) -> bool {
        *self.state.lock() == IS_SET
    }

    /// Sets the event.
    ///
    /// Setting the event will notify all pending waiters.
    pub fn set(&self) {
        let mut state = self.state.lock();
        let mut head = *state as *const WaitNode;
        *state = IS_SET;

        while !head.is_null() {
            let (new_head, tail) = unsafe { (&*head).dequeue() };
            tail.notify(false);
            head = new_head;
        }
    }

    /// Returns a future that gets fulfilled when the event is set.
    pub fn wait(&self) -> RawWaitForEventFuture<'_, Raw> {
        RawWaitForEventFuture {
            state: Some(&self.state),
            wait_node: WaitNode::default(),
        }
    }
}

/// A Future that is resolved once the corresponding ResetEvent has been set
pub struct RawWaitForEventFuture<'a, Raw: RawMutex> {
    state: Option<&'a Mutex<Raw, usize>>,
    wait_node: WaitNode,
}

unsafe impl<'a, Raw: RawMutex + Sync> Send for RawWaitForEventFuture<'a, Raw> {}

impl<'a, Raw: RawMutex> fmt::Debug for RawWaitForEventFuture<'a, Raw> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WaitForEventFuture").finish()
    }
}

impl<'a, Raw: RawMutex> FusedFuture for RawWaitForEventFuture<'a, Raw> {
    fn is_terminated(&self) -> bool {
        self.state.is_none()
    }
}

impl<'a, Raw: RawMutex> Drop for RawWaitForEventFuture<'a, Raw> {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            let mut state = state.lock();
            if self.wait_node.state.get() == WaitNodeState::Waiting {
                let head = (*state) as *const WaitNode;
                *state = self.wait_node.remove(head) as usize;
            }
        }
    }
}

impl<'a, Raw: RawMutex> Future for RawWaitForEventFuture<'a, Raw> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safe as RawWaitForEventFuture is !Unpin because of the WaitNode
        // and the address of the future wont change since it's pinned.
        let mut_self = unsafe { self.get_unchecked_mut() };
        let mut state = mut_self
            .state
            .expect("polled WaitForEventFuture after completion")
            .lock();

        match mut_self.wait_node.state.get() {
            WaitNodeState::Reset => {
                if *state == IS_SET {
                    mut_self.state = None;
                    Poll::Ready(())
                } else {
                    let head = (*state) as *const WaitNode;
                    let head = mut_self.wait_node.enqueue(head, ctx.waker().clone());
                    *state = head as usize;
                    Poll::Pending
                }
            }
            WaitNodeState::Waiting => Poll::Pending,
            WaitNodeState::Notified => {
                assert_eq!(*state, IS_SET);
                Poll::Ready(())
            }
        }
    }
}
