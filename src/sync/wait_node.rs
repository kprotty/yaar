use super::ThreadEvent;
use core::{
    mem::MaybeUninit,
    cell::Cell,
    ptr::null,
};

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum WaitNodeState {
    Uninit,
    Waiting,
    Notified,
    DirectNotified,
}

pub struct WaitNode<Event> {
    prev: Cell<MaybeUninit<*const Self>>,
    next: Cell<MaybeUninit<*const Self>>,
    tail: Cell<MaybeUninit<*const Self>>,
    event: Cell<MaybeUninit<Event>>,
    state: Cell<WaitNodeState>,
}

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
    pub fn init(&self, head: *const Self) {
        if self.state.get() == WaitNodeState::Uninit {
            self.state.set(WaitNodeState::Waiting);
            self.prev.set(MaybeUninit::new(null()));
            self.event.set(MaybeUninit::new(Event::default()));
        }

        self.next.set(MaybeUninit::new(head));
        if head.is_null() {
            self.tail.set(MaybeUninit::new(self));
        } else {
            self.tail.set(MaybeUninit::new(null()));
        }
    }

    pub fn set_prev(&self, new_prev: *const Self) {
        self.prev.set(MaybeUninit::new(new_prev))
    }

    pub unsafe fn get_prev(&self) -> *const Self {
        self.prev.get().assume_init()
    }

    pub fn set_tail(&self, new_tail: *const Self) {
        self.tail.set(MaybeUninit::new(new_tail))
    }

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
    fn get_event(&self) -> &Event {
        unsafe { &*(&*self.event.as_ptr()).as_ptr() }
    }

    pub fn reset(&self) {
        self.get_event().reset();
    }

    pub fn notify(&self, is_direct: bool) {
        self.state.set(if is_direct { WaitNodeState::DirectNotified } else { WaitNodeState::Notified });
        self.get_event().notify();
    }

    pub fn wait(&self) -> bool {
        self.get_event().wait();
        match self.state.get() {
            WaitNodeState::Notified => false,
            WaitNodeState::DirectNotified => true,
            state => unreachable!("WaitNode::wait() observed unexpected state: {:?}", state),
        }
    }
}