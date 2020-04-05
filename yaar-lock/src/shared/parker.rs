use crate::{YieldContext, AutoResetEvent, utils::{unreachable, UnwrapUnchecked}};
use core::{
    cell::Cell,
    ptr::{NonNull, drop_in_place},
    future::Future,
    marker::PhantomPinned,
    mem::{MaybeUninit, ManuallyDrop},
    sync::atomic::{fence, Ordering, AtomicUsize},
    task::{Poll, Task, Context, Waker, RawWaker, RawWakerVTable},
};

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct ParkToken(pub usize);

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct UnparkToken(pub usize);

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum ParkResult {
    Invalid,
    Cancelled,
    Unparked(UnparkToken),
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Default)]
pub struct UnparkResult {
    pub unparked: usize,
    pub skipped: usize,
    pub has_more: bool,
    _sealed: (),
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum UnparkFilter {
    Stop,
    Skip,
    Unpark(UnparkToken),
}

pub struct Parker<E> {
    state: AtomicUsize,
    queue: Cell<Option<NonNull<ParkWaiter<E>>>>,
}

impl<E> Parker<E> {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            queue: Cell::new(None),
        }
    }
}

impl<E: AutoResetEvent> Parker<E> {
    pub unsafe fn park<V, P, C>(
        &self,
        token: ParkToken,
        validate: impl FnOnce() -> bool,
        park: impl FnOnce(&E) -> Poll<bool>,
        cancel: impl FnOnce(bool),
    ) -> ParkFuture<'_, E, V, P, C>
    where
        V: FnOnce() -> bool,
        P: FnOnce(&E) -> Poll<bool>,
        C: FnOnce(bool),
    {
        ParkFuture {
            _pin: PhantomPinned,
            parker: self,
            validate: ManuallyDrop::new(validate),
            park: ManuallyDrop::new(park),
            cancel: ManuallyDrop::new(cancel),
            waiter: ParkWaiter::new(),
            waker: Cell::new(None),
            token: Cell::new(token.0),
        }
    }

    pub unsafe fn unpark(
        &self,
        filter: impl FnMut(ParkToken) -> UnparkFilter,
        callback: impl FnOnce(UnparkResult),
        unpark: impl FnMut(&E),
    ) -> UnparkResult {
        let mut unparked_list = UnparkList::new();
        let mut result = UnparkResult {
            unparked: 0,
            skipped: 0,
            has_more: false,
        };

        self.locked(&ParkWaiter::new(), |head| {
            let mut current = head.tail.get().assume_init();
            while let Some(waiter_ptr) = current {
                let waiter = &*waiter_ptr.as_ptr();
                current = waiter.prev.get().assume_init();
                result.has_more = current.is_some();

                match filter(ParkToken(waiter.token.get())) {
                    UnparkFilter::Stop => break,
                    UnparkFilter::Skip => result.skipped += 1,
                    UnparkFilter::Unpark(UnparkToken(token)) => {
                        Self::remove(head, waiter).unwrap();
                        waiter.token.set(token);

                    },
                }
            }
        });

        for waiter in unparked_list.waiters() {
        }
    }
}

struct UnparkList<E> {
    array_size: usize,
    array: [MaybeUninit<NonNull<ParkWaiter<E>>>; 16],
    overflow_head: Option<NonNull<ParkWaiter<E>>>,
    overflow_tail: Option<NonNull<ParkWaiter<E>>>,
}

impl<E> UnparkList<E> {
    pub const fn new() -> Self {
        Self {
            array_size: 0,
            array: unsafe { MaybeUninit::uninit() },
            overflow_head: None,
            overflow_tail: None,
        }
    }

    pub fn push(&mut self, waiter: &ParkWaiter<E>) {
        let ptr = NonNull::new(waiter as *const _ as *mut _);
        if self.array_size < self.array.len() {
            self.array[self.array_size] = MaybeUninit::new(ptr.unwrap());
            self.array_size += 1;
            return;
        }

        if let Some(tail) = self.overflow_tail {
            let tail = unsafe { &*tail.as_ptr() };
            tail.next.set(MaybeUninit::new(Some(ptr)));
        } else {
            self.overflow_head = MaybeUninit::new(ptr);
        }

        waiter.next.set(MaybeUninit::new(None));
        self.overflow_tail = MaybeUninit::new(ptr);
    }

    pub fn waiters(&self) -> impl Iterator<Item = &ParkWaiter<E>> + '_ {
        struct Iter<'a> {
            array_pos: usize,
            overflow_node: Option<NonNull<ParkWaiter<E>>>,
            list: &'a UnparkList,
        }

        impl<'a> Iterator for Iter<'a> {
            type Item = &'a ParkWaiter<E>;

            fn next(&mut self) -> Self::Item {
                unsafe {
                    if self.array_pos < self.list.array_size {
                        let waiter = *self.list.array.get_unchecked(self.array_pos);
                        self.array_pos += 1;
                        return Some(&*waiter.assume_init().as_ptr());
                    }

                    if let Some(waiter) = self.overflow_node {
                        let waiter = &*waiter.as_ptr();
                        self.overflow_node = waiter.next.get().assume_init();
                        return Some(waiter);
                    }

                    None
                }
            }
        }

        Iter {
            array_pos: 0,
            overflow_node: self.overflow_head,
            list: self,
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum ParkState {
    Uninit = 0,
    Waiting = 1,
    Updating = 2,
    Unparking = 3,
    Cancelling = 4,
    Unparked = 5,
}

impl From<usize> for ParkState {
    fn from(value: usize) -> Self {
        match value {
            0 => Self::Uninit,
            1 => Self::Waiting,
            2 => Self::Updating,
            3 => Self::Unparking,
            4 => Self::Cancelling,
            5 => Self::Unparked,
            _ => unsafe { unreachable() },
        }
    }
}

#[repr(align(4))]
struct ParkWaiter<E> {
    state: AtomicUsize,
    event: Cell<MaybeUninit<E>>,
    prev: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    next: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    tail: Cell<MaybeUninit<Option<NonNull<Self>>>>,
}

impl<E> Drop for ParkWaiter<E> {
    fn drop(&mut self) {
        let state = self.state.load(Ordering::Relaxed);
        if ParkState::from(state) != ParkState::Uninit {
            unsafe {
                let maybe_event = &mut *self.event.as_ptr();
                drop_in_place(maybe_event.as_mut_ptr());
            }
        }
    }
}

impl<E> ParkWaiter<E> {
    fn new() -> Self {
        Self {
            state: AtomicUsize::new(ParkState::Uninit as usize),
            event: Cell::new(MaybeUninit::uninit()),
            prev: Cell::new(MaybeUninit::uninit()),
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
        }
    }
}

pub struct ParkFuture<'a, E, V, P, C> {
    _pin: PhantomPinned,
    parker: &'a Parker<E>,
    validate: ManuallyDrop<V>,
    park: ManuallyDrop<P>,
    cancel: ManuallyDrop<C>,
    waiter: ParkWaiter<E>,
    waker: Cell<Option<Waker>>,
    token: Cell<usize>,
}

impl<'a, E, V, P, C> Drop for ParkFuture<'a, E, V, P, C> {
    fn drop(&mut self) {
        let state = self.waiter.state.load(Ordering::Relaxed);
        if ParkState::from(state) == ParkState::Waiting {
            self.cancel_park();
        } else {
            unsafe { self.cancel.drop() };
        }
    }
}

impl<'a, E, V, P, C> Future for ParkFuture<'a, E, V, P, C> {
    type Output = ParkResult;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut_self = 
    }
}

impl<'a, E, V, P, C> ParkFuture<'a, E, V, P, C> {
    fn cancel_park(&self) {
        if let Err(new_state) = self.waiter.state.compare_exchange(
            ParkState::Waiting as usize,
            ParkState::Cancelling as usize,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            assert_eq!(ParkState::from(new_state), ParkState::Unparked);
            return;
        }
    }
}

impl<E: AutoResetEvent> Parker<E> {
    const UNLOCKED: usize = 0b00;
    const LOCKED: usize   = 0b01;
    const DEQUEING: usize = 0b10;
    const MASK: usize = !(UNLOCKED | LOCKED | DEQUEING);

    fn locked<T>(
        &self,
        waiter: &ParkWaiter<E>,
        critical_section: impl FnOnce(&mut Option<NonNull<ParkWaiter<E>>>) -> T,
    ) -> T {
        // Acquire a lock on the queue using the state
        if let Err(state) = self.state.compare_exchange_weak(
            Self::UNLOCKED,
            Self::LOCKED,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            self.lock_slow(waiter, state);
        }

        // Safety:
        // The queue is locked during this operation
        // and is not get/set/deref'ed elsewhere in codebase without lock.
        let result = critical_section(unsafe { &*self.queue.as_ptr() });

        // Release the lock on the queue using the state
        let state = self.state.fetch_sub(Self::LOCKED, Ordering::Release);
        if (state & Self::MASK != 0) && (state & Self::DEQUEING == 0) {
            self.unlock_slow(state.wrapping_sub(Self::LOCKED));
        }

        result
    }

    #[cold]
    fn lock_slow(&self, waiter: &ParkWaiter<E>, mut state: usize) {
        let mut spin: usize = 0;
        let mut init_event = {
            let waiter_state = waiter.state.load(Ordering::Relaxed);
            ParkState::from(waiter_state) == ParkState::Uninit
        };

        loop {
            // Try to acquire the lock if its unlocked.
            if state & Self::LOCKED == 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state | Self::LOCKED,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Err(e) => state = e,
                    Ok(_) => return,
                }
                continue;
            }

            // Get the head of the waiter queue if any
            // and spin on the lock if the Event deems appropriate.
            let head = NonNull::new((state & Self::MASK) as *mut ParkWaiter<E>);
            if R::Event::yield_now(YieldContext {
                contended: head.is_some(),
                iteration: spin
            }) {
                spin = spin.wrapping_add(1);
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            // Lazily initialize the Event if its not already
            if init_event {
                init_event = false;
                waiter.prev.set(MaybeUninit::new(None));
                waiter.state.store(ParkState::Waiting as usize);
                waiter.event.set(MaybeUninit::new(R::Event::new()));
            }

            // Prepare the waiter to be enqueued in the wait queue.
            // If its the first waiter, set it's tail to itself.
            // If not, then the tail will be resolved by a dequeing thread.
            let waiter_ptr = waiter as *const _ as usize;
            waiter.next.set(MaybeUninit::new(head));
            waiter.tail.set(MaybeUninit::new(match head {
                Some(_) => None,
                None => NonNull::new(waiter_ptr as *mut _),
            }));

            // Try to enqueue the waiter as the new head of the wait queue.
            // Release barrier to make the waiter field writes visible to the deque thread.
            if let Err(e) = self.state.compare_exchange_weak(
                state,
                (state & !Self::MASK) | waiter_ptr,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                state = e;
                continue;
            }
            
            // Park on our event until unparked by a dequeing thread.
            // Safety: guaranteed to be initialized from init_event.
            unsafe {
                let maybe_event = &*node.event.as_ptr();
                let event_ref = &*maybe_event.as_ptr();
                event_ref.wait();
            }

            // Reset everything and try to acquire the lock again.
            spin = 0;
            node.prev.set(MaybeUninit::new(None));
            state = self.state.load(Ordering::Relaxed);
        }
    }

    #[cold]
    fn unlock_slow(&self, mut state: usize) {
        // The lock was released, try to deque a waiter and unpark then to reacquire the lock.
        // Stop trying if there are no waiters or if another thread is already dequeing.
        loop {
            if (state & Self::MASK == 0) || (state & Self::DEQUEING != 0) {
                return;
            }
            match self.state.compare_exchange_weak(
                state,
                state | Self::DEQUEING,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Err(e) => state = e,
                Ok(_) => {
                    state |= Self::DEQUEING;
                    break;
                },
            }
        }

        // Our thread is now the only one dequeing a node from the wait quuee.
        // On enter, and on re-loop, need an Acquire barrier in order to observe the head
        // waiter field writes from both the previous deque thread and the enqueue thread.
        'deque: loop {
            // Safety: guaranteed non-null from the DEQUEING acquire loop above.
            let head = unsafe { &*((state & Self::MASK) as *const ParkWaiter<E>) };

            // 
            let tail = unsafe {
                let mut current = head;
                loop {
                    if let Some(tail) = current.tail.get().assume_init() {
                        head.tail.set(MaybeUninit::new(Some(tail)));
                        break &*tail.as_ptr();
                    } else {
                        let next = current.next.get().assume_init();
                        let next = &*next.unwrap_unchecked().as_ptr();
                        let current_ptr = NonNull::new(current as *const _ as *mut _);
                        next.prev.set(MaybeUninit::new(current_ptr));
                        current = next;
                    }
                }
            };

            if state & Self::LOCKED != 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state & !Self::DEQUEING,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(e) => state = e,
                }
                fence(Ordering::Acquire);
                continue 'deque;
            }

            let new_tail = unsafe { tail.prev.get().assume_init() };
            
            if let Some(new_tail) = new_tail {
                head.tail.set(MaybeUninit::new(Some(new_tail)));
                self.state.fetch_sub(Self::DEQUEING, Ordering::Release);
            } else {
                loop {
                    match self.state.compare_exchange_weak(
                        state,
                        state & !(Self::MASK | Self::DEQUEING),
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => break,
                        Err(e) => state = e,
                    }
                    if state & Self::MASK != 0 {
                        fence(Ordering::Acquire);
                        continue 'deque;
                    }
                }
            }

            return unsafe {
                let maybe_event = &*tail.event.as_ptr();
                let event_ref = &*maybe_event.as_ptr();
                event_ref.set();
            };
        }
    }
}