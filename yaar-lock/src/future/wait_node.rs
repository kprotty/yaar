use core::{
    ptr::null,
    cell::Cell,
    task::Waker,
    marker::PhantomPinned,
};

/// The state of a waiting node in relation to the wait queue.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum WaitNodeState {
    /// The node is zero initialized and should check some resource.
    Reset,
    /// The node is initialized and is waiting in the wait queue.
    Waiting,
    /// The node was dequeued and given ownership of some resource.
    Notified,
}

/// An intrusive, doubly linked list node used to represent a suspended Future.
pub struct WaitNode {
    /// This is used to make any structures holding a WaitNode
    /// to be `!Unpin` as their memory shouldn't be moved after pinning.
    _pin: PhantomPinned,
    prev: Cell<*const Self>,
    next: Cell<*const Self>,
    tail: Cell<*const Self>,
    waker: Cell<Option<Waker>>,
    pub state: Cell<WaitNodeState>,
}

/// NOTE: using MaybeUninit didn't seem to give a performance increase.
impl Default for WaitNode {
    fn default() -> Self {
        Self {
            _pin: PhantomPinned,
            prev: Cell::new(null()),
            next: Cell::new(null()),
            tail: Cell::new(null()),
            waker: Cell::new(None),
            state: Cell::new(WaitNodeState::Reset),
        }
    }
}

impl WaitNode {
    /// Given the head of the wait_queue, push this current
    /// node onto the queue by returning the new queue head.
    ///
    /// Stores the waker for the wait node and supports being called
    /// more than once as its a pure function.
    pub fn enqueue(&self, head: *const Self, waker: Waker) -> *const Self {
        debug_assert_eq!(self.state.get(), WaitNodeState::Reset);
        self.next.set(head);
        self.prev.set(null());
        
        if head.is_null() {
            self.tail.set(self);
        } else {
            unsafe {
                (&*head).prev.set(self);
                self.tail.set((&*head).tail.get());
            }
        }

        self.state.set(WaitNodeState::Waiting);
        self.waker.replace(Some(waker));
        self as *const Self
    }

    /// Given the head of the wait queue as ourselves,
    /// return the new head of the queue + dequeue the tail node.
    pub fn dequeue<'a>(&self) -> (*const Self, &'a Self) {
        debug_assert_eq!(self.state.get(), WaitNodeState::Waiting);
        let tail = self.tail.get();

        // The current node is assumed to be initialized from an enqueue.
        unsafe {
            let new_tail = (&*tail).prev.get();
            if new_tail.is_null() {
                (null(), &*tail)
            } else {
                self.tail.set(new_tail);
                (self as *const Self, &*tail)
            }
        }
    }

    /// Given the head of the wait queue, dequeue ourselves
    /// from the queue as we were cancelled.
    pub fn remove(&self, head: *const Self) -> *const Self {
        debug_assert_eq!(self.state.get(), WaitNodeState::Waiting);
        debug_assert!(!head.is_null());
        let prev = self.prev.get();
        let next = self.next.get();

        // We are assumed to be initialized from being enqueued.
        unsafe {
            // Fix the links of nodes around us
            if !prev.is_null() {
                (&*prev).next.set(next);
            }
            if !next.is_null() {
                (&*next).prev.set(prev);
            }
            // return the new head of the queue if changed.
            if head == (self as *const Self) {
                null()
            } else {
                // fix the tail of the queue if it was us.
                if (&*head).tail.get() == (self as *const Self) {
                    (&*head).tail.set(prev);
                }
                head
            }
        }
    }

    /// Given a dequeud node as ourselves,
    /// wake up our Waker using the appropriate state based on is_direct.
    pub fn notify(&self, is_direct: bool) {
        debug_assert_eq!(self.state.get(), WaitNodeState::Waiting);
        self.state.set(if is_direct {
            WaitNodeState::Notified
        } else {
            WaitNodeState::Reset
        });

        self.waker
            .replace(None)
            .expect("Tried to notify a non waiting node")
            .wake()
    }
}