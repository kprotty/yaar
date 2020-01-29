use crate::ThreadEvent;
use core::{cell::Cell, mem::MaybeUninit, ptr::null};

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum WaitNodeState {
    Uninit,
    Waiting,
    Notified,
    DirectNotified,
}

pub struct WaitNode<E> {
    state: Cell<WaitNodeState>,
    event: Cell<MaybeUninit<E>>,
    prev: Cell<MaybeUninit<*const Self>>,
    next: Cell<MaybeUninit<*const Self>>,
    tail: Cell<MaybeUninit<*const Self>>,
}

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
    pub fn enqueue(&self, head: *const Self) -> *const Self {
        // lazy initialize a node before prepending to the head
        match self.state.get() {
            WaitNodeState::Uninit => {
                self.state.set(WaitNodeState::Waiting);
                self.prev.set(MaybeUninit::new(null()));
                self.event.set(MaybeUninit::new(E::default()));
            },
            WaitNodeState::Waiting => {},
            #[cfg_attr(not(debug_assertions), allow(unused_variables))]
            unexpected => {
                #[cfg(not(debug_assertions))]
                unsafe {
                    core::hint::unreachable_unchecked()
                }
                #[cfg(debug_assertions)]
                unreachable!(
                    "unexpected WaitNodeState: expected {:?} found {:?}",
                    WaitNodeState::Waiting,
                    unexpected,
                );
            },
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
    #[inline]
    fn get_event(&self) -> &E {
        debug_assert_eq!(self.state.get(), WaitNodeState::Waiting);
        unsafe { &*(&*self.event.as_ptr()).as_ptr() }
    }

    pub fn reset(&self) {
        self.get_event().reset();
        self.state.set(WaitNodeState::Waiting);
        self.prev.set(MaybeUninit::new(null()));
    }

    pub fn notify(&self, is_direct: bool) {
        let event = self.get_event();
        debug_assert_eq!(self.state.get(), WaitNodeState::Waiting);
        self.state.set(if is_direct {
            WaitNodeState::DirectNotified
        } else {
            WaitNodeState::Notified
        });
        event.notify();
    }

    pub fn wait(&self) -> bool {
        self.get_event().wait();
        match self.state.get() {
            WaitNodeState::Notified => false,
            WaitNodeState::DirectNotified => true,
            #[cfg_attr(not(debug_assertions), allow(unused_variables))]
            unexpected => {
                #[cfg(not(debug_assertions))]
                unsafe {
                    core::hint::unreachable_unchecked()
                }
                #[cfg(debug_assertions)]
                unreachable!(
                    "unexpected WaitNodeState: expected {:?} or {:?} found {:?}",
                    WaitNodeState::Notified,
                    WaitNodeState::DirectNotified,
                    unexpected,
                );
            },
        }
    }
}
