use crate::ThreadEvent;
use core::{cell::Cell, mem::MaybeUninit, ptr::null};

/// The state of a WaitNode in relation to the wait queue.
#[derive(Debug, Copy, Clone, PartialEq)]
pub(crate) enum WaitNodeState {
    /// The node is uninitialized so reading from the fields is UB
    Uninit,
    /// The node is initialized and probably waiting in the wait queue.
    Waiting,
    /// The node was dequeued and should check a resource again.
    Notified,
    /// The node was dequeued and was given ownership of a resource.
    DirectNotified,
}

/// An intrusive, doubly linked list node used to track blocked threads.
pub(crate) struct WaitNode<E> {
    state: Cell<WaitNodeState>,
    event: Cell<MaybeUninit<E>>,
    prev: Cell<MaybeUninit<*const Self>>,
    next: Cell<MaybeUninit<*const Self>>,
    tail: Cell<MaybeUninit<*const Self>>,
}

/// Lazy initialize WaitNode as it improves performance in the fast path.
impl<E> Default for WaitNode<E> {
    fn default() -> Self {
        Self {
            state: Cell::new(WaitNodeState::Uninit),
            event: Cell::new(MaybeUninit::uninit()),
            prev: Cell::new(MaybeUninit::uninit()),
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
        }
    }
}

impl<E: Default> WaitNode<E> {
    /// Given the head of the queue, prepend this WaitNode
    /// to the queue by initializing it and returning the
    /// new head of the queue.
    pub fn enqueue(&self, head: *const Self) -> *const Self {
        match self.state.get() {
            // lazy initialize a node before prepending to the head
            WaitNodeState::Uninit => {
                self.state.set(WaitNodeState::Waiting);
                self.prev.set(MaybeUninit::new(null()));
                self.event.set(MaybeUninit::new(E::default()));
            }
            // node is already initialized, only change the links volatile to the head below.
            WaitNodeState::Waiting => {}
            // node is in an unknown state, unchecked in release for performance (less so than in
            // notify())
            #[cfg(not(debug_assertions))]
            _ => unsafe { core::hint::unreachable_unchecked() },
            // In debug mode, this fault should still be caught and reported
            #[cfg(debug_assertions)]
            unexpected => unreachable!(
                "unexpected WaitNodeState: expected {:?} found {:?}",
                WaitNodeState::Waiting,
                unexpected,
            ),
        }

        // prepare a node to be the new head of the queue
        self.next.set(MaybeUninit::new(head));
        if head.is_null() {
            self.tail.set(MaybeUninit::new(self));
        } else {
            self.tail.set(MaybeUninit::new(null()));
        }

        // return ourselves as the new head
        self as *const Self
    }

    /// Given the head of the queeu as ourselves,
    /// dequeue a node from the queue returning the new tail
    /// of the queue and the removed tail that was dequeued.
    ///
    /// This function is not pure like `enqueue()` and it modifies
    /// the internal queue tail for tracking the tail node.
    pub fn dequeue<'a>(&self) -> (*const Self, &'a Self) {
        unsafe {
            // Given the head of the queue
            let head = self;
            debug_assert_eq!(head.state.get(), WaitNodeState::Waiting);

            // Find the tail, updating the links along the way
            let mut current = head;
            let mut tail = head.tail.get().assume_init();
            while tail.is_null() {
                let next = &*current.next.get().assume_init();
                debug_assert_eq!((&*next).state.get(), WaitNodeState::Waiting);
                next.prev.set(MaybeUninit::new(current));
                tail = next.tail.get().assume_init();
                current = next;
            }

            // Dequeue the tail, returning the new_tail and it.
            debug_assert_eq!((&*tail).state.get(), WaitNodeState::Waiting);
            let new_tail = (&*tail).prev.get().assume_init();
            if (head as *const _) == tail {
                (null(), &*tail)
            } else {
                head.tail.set(MaybeUninit::new(new_tail));
                (new_tail, &*tail)
            }
        }
    }
}

impl<E: ThreadEvent> WaitNode<E> {
    /// Get a reference to the thread event, assuming the WaitNode is
    /// initialized.
    #[inline]
    fn get_event(&self) -> &E {
        unsafe { &*(&*self.event.as_ptr()).as_ptr() }
    }

    /// Reset the wait node without uninitializing it.
    /// Less expensive than re-initialization, especially for larger
    /// ThreadEvent's.
    pub fn reset(&self) {
        self.get_event().reset();
        self.state.set(WaitNodeState::Waiting);
        self.prev.set(MaybeUninit::new(null()));
    }

    /// Unblock this node, waking it up with either normal or direct notify.
    /// This assumes that this WaitNode is in a waiting state.
    pub fn notify(&self, is_direct: bool) {
        let event = self.get_event();
        debug_assert_eq!(self.state.get(), WaitNodeState::Waiting);
        self.state.set(if is_direct {
            WaitNodeState::DirectNotified
        } else {
            WaitNodeState::Notified
        });
        event.set();
    }

    /// Block this node, waiting to be notified by another WaitNode.
    /// Returns whether the notification was direct.
    /// This assumes that this node is initialized.
    pub fn wait(&self) -> bool {
        self.get_event().wait();
        match self.state.get() {
            WaitNodeState::Notified => false,
            WaitNodeState::DirectNotified => true,
            // Using unreachable_unchecked improves performance during benchmarks.
            #[cfg(not(debug_assertions))]
            _ => unsafe { core::hint::unreachable_unchecked() },
            // In debug mode, this fault should still be caught and reported
            #[cfg(debug_assertions)]
            unexpected => unreachable!(
                "unexpected WaitNodeState: expected {:?} or {:?} found {:?}",
                WaitNodeState::Notified,
                WaitNodeState::DirectNotified,
                unexpected,
            ),
        }
    }
}
