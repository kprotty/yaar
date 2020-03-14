use core::{
    ptr::null,
    cell::Cell,
    mem::{drop, MaybeUninit},
};

#[derive(Copy, Clone, PartialEq)]
enum WaiterState {
    Uninit,
    Enqueued,
    Processed,
    Dequeued,
}

pub struct Waiter<Signal> {
    state: Cell<WaiterState>,
    signal: Cell<MaybeUninit<Signal>>,
    prev: Cell<MaybeUninit<*const Self>>,
    next: Cell<MaybeUninit<*const Self>>,
    tail: Cell<MaybeUninit<*const Self>>,
}

impl<Signal> Drop for Waiter<Signal> {
    fn drop(&mut self) {
        let state = self.state.get();
        if state != WaiterState::Uninit {
            drop(unsafe { self.signal.get().assume_init() });
        }
        debug_assert_ne!(state, WaiterState::Processed, "Waiter dropped while in queue");
    }
}

impl<Signal> Waiter<Signal> {
    pub const fn new() -> Self {
        Self {
            state: Cell::new(WaiterState::Uninit),
            signal: Cell::new(MaybeUninit::uninit()),
            prev: Cell::new(MaybeUninit::uninit()),
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
        }
    }

    pub unsafe fn signal(&self) -> &Signal {
        debug_assert_ne!(self.state.get(), WaiterState::Uninit);
        &*(&*self.signal.as_ptr()).as_ptr()
    }

    pub unsafe fn enqueue(
        &self,
        head: *const Self,
        init_signal: &Cell<MaybeUninit<impl FnOnce() -> Signal>>,
    ) -> *const Self {
        match self.state.get() {
            WaiterState::Uninit => {
                self.state.set(WaiterState::Enqueued);
                self.prev.set(MaybeUninit::new(null()));
                self.signal.set(MaybeUninit::new(unsafe {
                    init_signal.replace(MaybeUninit::uninit()).assume_init()()
                }));
            },
            WaiterState::Enqueued => {},
            WaiterState::Processed => unreachable!("Trying to enqueue() a Waiter already in queue"),
            WaiterState::Dequeue => {
                self.prev.set(MaybeUninit::new(null()));
                self.state.set(WaiterState::Enqueue);
            },
        }


        self.next.set(MaybeUninit::new(head));
        if head.is_null() {
            self.state.set(WaiterState::Processed);
            self.tail.set(MaybeUninit::new(self));
        } else {
            self.tail.set(MaybeUninit::new(null()));
        }

        self
    }

    pub unsafe fn find_tail<'a>(&self) -> &'a Self {
        let head = self;
        debug_assert!(matches!(head.state.get(), WaiterState::Enqueued | WaiterState::Processed));

        let mut tail = head.tail.get().assume_init();
        if !tail.is_null() {
            return &*tail;
        }

        let mut current = head;
        while tail.is_null() {
            let next = current.next.get().assume_init();
            debug_assert!(!next.is_null());

            let next = &*next;
            debug_assert!(matches!(next.state.get(), WaiterState::Enqueued | WaiterState::Processed));
            tail = next.tail.get().assume_init();

            next.state.set(WaiterState::Processed);
            next.prev.set(MaybeUninit::new(current));
            current = next;
        }

        head.state.set(WaiterState::Processed);
        head.tail.set(MaybeUninit::new(tail));
        &*tail
    }

    pub unsafe fn dequeue(&self, tail: &Self) -> bool {
        let head = self;
        debug_assert_eq!(head.state.get(), WaiterState::Processed);
        tail.state.set(WaiterState::Dequeued);
        head.tail.set(tail.prev.get());
        (head as *const _ as usize) == (tail as *const _ as usize)
    }

    pub unsafe fn reset(&self, tail: &Self) {
        let head = self;
        debug_assert_eq!(head as *const _ as usize, tail as *const _ as usize);
        debug_assert_eq!((&*head).state.get(), WaiterState::Dequeued);
        head.tail.set(MaybeUninit::new(head));
    }
}