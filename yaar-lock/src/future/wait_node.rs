use core::{
    ptr::null,
    cell::Cell,
    task::Waker,
    marker::PhantomPinned,
};

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum WaitNodeState {
    Reset,
    Waiting,
    Notified,
}

pub struct WaitNode {
    _pin: PhantomPinned,
    prev: Cell<*const Self>,
    next: Cell<*const Self>,
    tail: Cell<*const Self>,
    waker: Cell<Option<Waker>>,
    pub state: Cell<WaitNodeState>,
}

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
        self.waker.set(Some(waker));
        self as *const Self
    }

    pub fn dequeue<'a>(&self) -> (*const Self, &'a Self) {
        debug_assert_eq!(self.state.get(), WaitNodeState::Waiting);
        let tail = self.tail.get();

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

    pub fn remove(&self, head: *const Self) -> *const Self {
        debug_assert_eq!(self.state.get(), WaitNodeState::Waiting);
        debug_assert!(!head.is_null());
        let prev = self.prev.get();
        let next = self.next.get();

        unsafe {
            if !prev.is_null() {
                (&*prev).next.set(next);
            }
            if !next.is_null() {
                (&*next).prev.set(prev);
            }
            if head == (self as *const Self) {
                null()
            } else {
                if (&*head).tail.get() == (self as *const Self) {
                    (&*head).tail.set(prev);
                }
                head
            }
        }
    }

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