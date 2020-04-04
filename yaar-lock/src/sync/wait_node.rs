use crate::ThreadEvent;
use core::{cell::Cell, mem::MaybeUninit, ptr::null};

#[derive(Copy, Clone, Debug, PartialEq)]
enum State {
    Uninit,
    Waiting,
    Notified,
}

pub struct WaitNode<E, T> {
    pub tag: T,
    state: Cell<State>,
    event: Cell<MaybeUninit<E>>,
    prev: Cell<MaybeUninit<*const Self>>,
    next: Cell<MaybeUninit<*const Self>>,
    tail: Cell<MaybeUninit<*const Self>>,
}

impl<E: Default, T> WaitNode<E, T> {
    pub fn new(tag: T) -> Self {
        Self {
            tag,
            state: Cell::new(State::Uninit),
            event: Cell::new(MaybeUninit::uninit()),
            prev: Cell::new(MaybeUninit::uninit()),
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
        }
    }

    pub fn push(&self, head: *const Self) -> *const Self {
        if self.state.get() == State::Uninit {
            self.state.set(State::Waiting);
            self.prev.set(MaybeUninit::new(null()));
            self.event.set(MaybeUninit::new(E::default()));
        }

        self.next.set(MaybeUninit::new(head));
        if head.is_null() {
            self.tail.set(MaybeUninit::new(self));
        } else {
            self.tail.set(MaybeUninit::new(null()));
        }

        let new_head = self as *const Self;
        new_head
    }

    pub fn tail<'a>(&self) -> &'a Self {
        unsafe {
            let head = self;
            let mut tail = head.tail.get().assume_init();

            if tail.is_null() {
                let mut node = head;
                while tail.is_null() {
                    let next = &*node.next.get().assume_init();
                    next.prev.set(MaybeUninit::new(node));
                    node = next;
                    tail = node.tail.get().assume_init();
                }
                head.tail.set(MaybeUninit::new(tail));
            }

            &*tail
        }
    }

    pub fn next(&self) -> *const Self {
        let tail = self;
        unsafe { tail.prev.get().assume_init() }
    }

    pub fn pop(&self, new_tail: *const Self) {
        let head = self;
        if !new_tail.is_null() {
            head.tail.set(MaybeUninit::new(new_tail));
        }
    }
}

impl<E: ThreadEvent, T> WaitNode<E, T> {
    #[inline(always)]
    fn event(&self) -> &E {
        unsafe { &*(&*self.event.as_ptr()).as_ptr() }
    }

    pub fn reset(&self) {
        self.prev.set(MaybeUninit::new(null()));
        self.event().reset();
    }

    pub fn wait(&self) {
        self.event().wait();
    }

    pub fn notify(&self) {
        self.state.set(State::Notified);
        self.event().set();
    }
}
