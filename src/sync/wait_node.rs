use super::ThreadEvent;
use core::{cell::Cell, mem::MaybeUninit, ptr::null};

/// The state of a node waiting in a wait queue.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum WaitNodeState {
    /// The node has not been initialized, so the MaybeUninit fields are empty.
    Uninit,
    /// The node has been initialized and is waiting in the wait queue.
    Waiting,
    /// The node was dequeued from the wait queue and woken up.
    Notified,
    /// The node was dequeued & woken up, but now owning a resource.
    DirectNotified,
}

/// A doubly-linked list node representing a blocked thread in a wait queue.
pub struct WaitNode<Event> {
    prev: Cell<MaybeUninit<*const Self>>,
    next: Cell<MaybeUninit<*const Self>>,
    tail: Cell<MaybeUninit<*const Self>>,
    event: Cell<MaybeUninit<Event>>,
    state: Cell<WaitNodeState>,
}

/// WaitNode initialization uses MaybeUninit in order
/// to avoid memory writes and lazily initialize instead.
/// This is due to the event being possibly expensive to create.
impl<Event> Default for WaitNode<Event> {
    fn default() -> Self {
        Self {
            prev: Cell::new(MaybeUninit::uninit()),
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
            event: Cell::new(MaybeUninit::uninit()),
            state: Cell::new(WaitNodeState::Uninit),
        }
    }
}

impl<Event: Default> WaitNode<Event> {
    /// Initialize the wait node fields given the head of the queue.
    pub fn init(&self, head: *const Self) {
        // lazy initialize the event and change the state here.
        if self.state.get() == WaitNodeState::Uninit {
            self.state.set(WaitNodeState::Waiting);
            self.prev.set(MaybeUninit::new(null()));
            self.event.set(MaybeUninit::new(Event::default()));
        }

        // prepare this node to be the new head of the queue.
        self.next.set(MaybeUninit::new(head));
        if head.is_null() {
            self.tail.set(MaybeUninit::new(self));
        } else {
            self.tail.set(MaybeUninit::new(null()));
        }
    }

    /// Set the previous node pointer
    pub fn set_prev(&self, new_prev: *const Self) {
        self.prev.set(MaybeUninit::new(new_prev))
    }

    /// Get the previous node pointer.
    /// Assumes that the node has been initialized using `init()`.
    pub unsafe fn get_prev(&self) -> *const Self {
        self.prev.get().assume_init()
    }

    /// Set the tail pointer of the node.
    pub fn set_tail(&self, new_tail: *const Self) {
        self.tail.set(MaybeUninit::new(new_tail))
    }

    /// Get the tail pointer of the node.
    /// Assumes that the node has been initialized using `init()`.
    ///
    /// Given the head of the queue, find the tail node.
    /// The first node to enter the queue will set its tail to itself.
    /// Later node entries in the queue then set their tail to null.
    ///
    /// By starting from the head pointer and following the next links,
    /// we can eventually find the tail of the queue while setting the
    /// previous links of nodes we pass to form a doubly-linked list.
    ///
    /// The tail of the queue is then cached on the head so that subsequent
    /// tail lookups dont have to scan through as many nodes since the tail
    /// node will be reached much earlier.
    pub unsafe fn get_tail<'a>(&self) -> *const Self {
        let mut current = self;
        loop {
            let tail = current.tail.get().assume_init();
            if tail.is_null() {
                let next = &*current.next.get().assume_init();
                next.prev.set(MaybeUninit::new(current));
                current = next;
            } else {
                self.set_tail(tail);
                return tail;
            }
        }
    }
}

impl<Event: ThreadEvent> WaitNode<Event> {
    /// Get a reference tothe lazily initialized thread event.
    fn get_event(&self) -> &Event {
        unsafe { &*(&*self.event.as_ptr()).as_ptr() }
    }

    /// Reset the thread event so that it can be wait()'ed on again.
    pub fn reset(&self) {
        debug_assert!(match self.state.get() {
            WaitNodeState::Notified | WaitNodeState::DirectNotified => true,
            state => panic!(
                "WaitNode::reset() called with unexpected state: {:?}",
                state
            ),
        });
        self.get_event().reset();
    }

    /// Notify and wake up a blocked thread waiting on our thread event.
    pub fn notify(&self, is_direct: bool) {
        debug_assert!(match self.state.get() {
            WaitNodeState::Waiting => true,
            state => panic!(
                "WaitNode::notify() called with unexpected state {:?}",
                state
            ),
        });
        self.state.set(if is_direct {
            WaitNodeState::DirectNotified
        } else {
            WaitNodeState::Notified
        });
        self.get_event().notify();
    }

    /// Wait for the thread event to be notified and unblocking from another
    /// thread.
    pub fn wait(&self) -> bool {
        debug_assert!(match self.state.get() {
            WaitNodeState::Waiting => true,
            state => panic!("WaitNode::wait() called with unexpected state {:?}", state),
        });
        self.get_event().wait();
        match self.state.get() {
            WaitNodeState::Notified => false,
            WaitNodeState::DirectNotified => true,
            #[cfg(debug_assertions)]
            state => unreachable!(
                "WaitNode::wait() notified with unexpected state: {:?}",
                state
            ),
            #[cfg(not(debug_assertions))]
            _ => unsafe { core::hint::unreachable_unchecked() },
        }
    }
}
