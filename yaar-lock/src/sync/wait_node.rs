use crate::ThreadEvent;
use core::{cell::Cell, mem::MaybeUninit, ptr::null};

#[derive(Copy, Clone, Debug, PartialEq)]
enum State {
    Uninit,
    Waiting,
    Notified,
}

pub struct WaitNode<E, Token> {
    state: Cell<State>,
    event: Cell<MaybeUninit<E>>,
    token: Cell<MaybeUninit<Token>>,
    prev: Cell<MaybeUninit<*const Self>>,
    next: Cell<MaybeUninit<*const Self>>,
    tail: Cell<MaybeUninit<*const Self>>,
}

impl<E: Default, Token> WaitNode<E, Token> {
    pub fn new() -> Self {
        Self {
            state: Cell::new(State::Uninit),
            event: Cell::new(MaybeUninit::uninit()),
            token: Cell::new(MaybeUninit::uninit()),
            prev: Cell::new(MaybeUninit::uninit()),
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
        }
    }

    pub fn push(&self, head: *const Self, is_locked: bool) -> *const Self {
        if self.state.get() == State::Uninit {
            self.state.set(State::Waiting);
            self.prev.set(MaybeUninit::new(null()));
            self.event.set(MaybeUninit::new(E::default()));
        }

        debug_assert_eq!(self.state.get(), State::Waiting);
        self.next.set(MaybeUninit::new(head));
        if head.is_null() {
            self.tail.set(MaybeUninit::new(self));
        } else if !is_locked {
            self.tail.set(MaybeUninit::new(null()));
        } else {
            unsafe {
                let head = &*head;
                self.tail.set(head.tail.get());
                head.prev.set(MaybeUninit::new(self));
            }
        }

        let new_head = self as *const Self;
        new_head
    }

    pub fn tail<'a>(&self) -> &'a Self {
        unsafe {
            debug_assert_eq!(self.state.get(), State::Waiting);
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

    pub fn pop(&self, tail: &Self) -> *const Self {
        debug_assert_ne!(self.state.get(), State::Uninit);
        let new_tail = unsafe { tail.prev.get().assume_init() };

        let head = self;
        if !new_tail.is_null() {
            head.tail.set(MaybeUninit::new(new_tail));
        }

        new_tail
    }
}

impl<E: ThreadEvent, Token> WaitNode<E, Token> {
    #[inline(always)]
    fn event(&self) -> &E {
        debug_assert_ne!(self.state.get(), State::Uninit);
        unsafe { &*(&*self.event.as_ptr()).as_ptr() }
    }

    pub fn reset(&self) {
        debug_assert_eq!(self.state.get(), State::Notified);
        self.prev.set(MaybeUninit::new(null()));
        self.event().reset();
    }

    pub fn wait(&self) -> Token {
        debug_assert_eq!(self.state.get(), State::Waiting);
        self.event().wait();

        debug_assert_eq!(self.state.get(), State::Notified);
        unsafe { self.token.replace(MaybeUninit::uninit()).assume_init() }
    }

    pub fn notify(&self, token: Token) {
        debug_assert_eq!(self.state.get(), State::Waiting);
        self.token.set(MaybeUninit::new(token));

        self.state.set(State::Notified);
        self.event().set();
    }
}
