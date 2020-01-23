mod event;
mod lock;

pub use self::event::WordEvent;
pub use self::lock::WordLock;

use core::{cell::Cell, mem::{self, MaybeUninit}, marker::PhantomPinned};

/// Flag on WaitNode indicating that the waker was initialized.
pub const WAIT_NODE_INIT: u8 = 1 << 0;

/// Flag on WaitNode indicating that the previous lock holder
/// moved ownership of the lock to our node.
pub const WAIT_NODE_HANDOFF: u8 = 1 << 2;

/// Intrusive linked list node representing a suspended
/// unit of execution waiting for a lock to be released.
pub struct WaitNode<Waker> {
    pub prev: Cell<MaybeUninit<*const Self>>,
    pub next: Cell<MaybeUninit<*const Self>>,
    pub tail: Cell<MaybeUninit<*const Self>>,
    pub waker: Cell<MaybeUninit<Waker>>,
    pub flags: Cell<u8>,
    _pin: PhantomPinned,
}

/// Make sure to drop the waker if it
/// somehow remains alive after being waken.
impl<Waker> Drop for WaitNode<Waker> {
    fn drop(&mut self) {
        if self.flags.get() & WAIT_NODE_INIT != 0 {
            let waker = self.waker.replace(MaybeUninit::uninit());
            mem::drop(unsafe { waker.assume_init() });
        }
    }
}

/// Use `MaybeUninit::uninit()` to initialize the fields
/// as it optimizes away the memory writes for fast paths.
impl<Waker> Default for WaitNode<Waker> {
    fn default() -> Self {
        Self {
            prev: Cell::new(MaybeUninit::uninit()),
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
            waker: Cell::new(MaybeUninit::uninit()),
            flags: Cell::new(0),
            _pin: PhantomPinned,
        }
    }
}

impl<Waker> WaitNode<Waker> {
    /// Given the head of the queue of waiting nodes, find the tail node.
    ///
    /// The first node to enter the queue will set its tail to itself.
    /// Later node entries in the queue then set their tail to null.
    ///
    /// By starting from the head pointer and following the next links,
    /// we can eventually find the tail of the queue while settting the
    /// previous links of nodes we pass to form a doubly-linked list.
    ///
    /// The tail of the queue is then cached on the head so that subsequent
    /// tail lookups dont have to scan through as many nodes since the head
    /// node will be reached much earlier.
    pub fn find_tail<'a>(&self) -> &'a Self {
        unsafe {
            let mut current = self;
            loop {
                let tail = current.tail.get().assume_init();
                if tail.is_null() {
                    let next = &*current.next.get().assume_init();
                    next.prev.set(MaybeUninit::new(current));
                    current = next;
                } else {
                    self.tail.set(MaybeUninit::new(tail));
                    return &*tail;
                }
            }
        }
    }
}
