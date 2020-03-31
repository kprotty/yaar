use super::{AutoResetEvent, Lock};
use core::{
    cell::Cell,
    future::Future,
    marker::PhantomPinned,
    mem::MaybeUninit,
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum ParkResult {
    Unprepared,
    Unparked(usize),
    Cancelled(usize),
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum UnparkResult {
    Stop,
    Skip,
    Unpark,
}

#[derive(Debug)]
pub struct UnparkContext {
    pub has_more: bool,
    pub unparked: usize,
}

pub struct Parker<Event> {
    queue: Lock<Event, Option<NonNull<ParkNode<Event>>>>,
}

impl<Event> Parker<Event> {
    pub const fn new() -> Self {
        Self {
            queue: Lock::new(None),
        }
    }
}

impl<Event: AutoResetEvent> Parker<Event> {
    pub async unsafe fn park(
        &self,
        wait: impl FnOnce(&Event) -> Poll<bool>,
        prepare: impl FnOnce() -> Result<usize, ()>,
        cancel: impl FnOnce(usize, bool),
    ) -> ParkResult {
        let future = ParkFuture::new(self, wait, cancel);
        let node = &future.node;

        match self.queue.locked(&node.event, |head| {
            prepare().map(|token| {
                let node_ptr = NonNull::new(node as *const _ as *mut _);
                node.next.set(*head);
                node.prev.set(None);
                node.tail.set(match head.map(|p| &*p.as_ptr()) {
                    None => node_ptr,
                    Some(head) => {
                        head.prev.set(node_ptr);
                        head.tail.get()
                    }
                });
                node.token.set(token);
                *head = node_ptr;
            })
        }) {
            Ok(_) => future.await,
            Err(_) => ParkResult::Unprepared,
        }
    }

    pub unsafe fn unpark_one(
        &self,
        notify: impl FnMut(&Event),
        mut callback: impl FnMut(&UnparkContext, &mut usize),
    ) {
        let mut has_unparked = false;
        self.unpark(notify, |context, token| {
            if core::mem::replace(&mut has_unparked, false) {
                callback(context, token);
                UnparkResult::Unpark
            } else {
                UnparkResult::Stop
            }
        })
    }

    pub unsafe fn unpark(
        &self,
        mut notify: impl FnMut(&Event),
        mut filter: impl FnMut(&UnparkContext, &mut usize) -> UnparkResult,
    ) {
        let notify_list = self.queue.locked(&Event::default(), |head| {
            let mut current = *head;
            let mut notify_list = UnparkList::new();
            let mut context = UnparkContext {
                has_more: true,
                unparked: 0,
            };

            while let Some(node) = current {
                let node = &*node.as_ptr();
                current = node.prev.get();
                context.has_more = current.is_some();

                match filter(&context, &mut *node.token.as_ptr()) {
                    UnparkResult::Stop => break,
                    UnparkResult::Skip => {}
                    UnparkResult::Unpark => {
                        Self::remove(head, node).unwrap();
                        context.unparked += 1;
                        if let ParkState::Waiting = ParkState::from(
                            node.state
                                .swap(ParkState::Unparked as usize, Ordering::Acquire),
                        ) {
                            let ptr = node as *const _ as *mut _;
                            notify_list.push(NonNull::new_unchecked(ptr));
                        }
                    }
                }
            }

            notify_list
        });

        for node in notify_list.iter() {
            let node = &*node.as_ptr();
            notify(&node.event);
            if let Some(waker) = node.waker.replace(None) {
                waker.wake();
            }
        }
    }

    pub(self) unsafe fn remove(
        head: &mut Option<NonNull<ParkNode<Event>>>,
        node: &ParkNode<Event>,
    ) -> Result<bool, ()> {
        let tail = node.tail.replace(None);
        if tail.is_none() {
            return Err(());
        }

        let prev = node.prev.get();
        let next = node.next.get();
        if let Some(prev) = prev {
            (&*prev.as_ptr()).next.set(next);
        }
        if let Some(next) = next {
            (&*next.as_ptr()).prev.set(prev);
        }

        if let Some(head_node) = *head {
            let node_ptr = NonNull::new_unchecked(node as *const _ as *mut _);
            if head_node == node_ptr {
                *head = next;
            } else if tail == Some(node_ptr) {
                (&*head_node.as_ptr()).tail.set(prev);
            }
        }

        let was_last = (*head).is_none();
        Ok(was_last)
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum ParkState {
    Waiting = 0,
    Updating = 1,
    Unparked = 2,
    Cancelled = 3,
}

impl From<usize> for ParkState {
    fn from(value: usize) -> Self {
        match value & 0b11 {
            0 => Self::Waiting,
            1 => Self::Updating,
            2 => Self::Unparked,
            3 => Self::Cancelled,
            _ => unreachable!(),
        }
    }
}

struct ParkNode<Event> {
    _pin: PhantomPinned,
    event: Event,
    state: AtomicUsize,
    token: Cell<usize>,
    waker: Cell<Option<Waker>>,
    prev: Cell<Option<NonNull<Self>>>,
    next: Cell<Option<NonNull<Self>>>,
    tail: Cell<Option<NonNull<Self>>>,
}

struct ParkFuture<'a, Event, WaitFn, CancelFn>
where
    Event: AutoResetEvent,
    WaitFn: FnOnce(&Event) -> Poll<bool>,
    CancelFn: FnOnce(usize, bool),
{
    node: ParkNode<Event>,
    parker: &'a Parker<Event>,
    wait_fn: Cell<Option<WaitFn>>,
    cancel_fn: Cell<Option<CancelFn>>,
}

impl<'a, Event, WaitFn, CancelFn> ParkFuture<'a, Event, WaitFn, CancelFn>
where
    Event: AutoResetEvent,
    WaitFn: FnOnce(&Event) -> Poll<bool>,
    CancelFn: FnOnce(usize, bool),
{
    pub fn new(parker: &'a Parker<Event>, wait_fn: WaitFn, cancel_fn: CancelFn) -> Self {
        Self {
            node: ParkNode {
                _pin: PhantomPinned,
                event: Event::default(),
                state: AtomicUsize::new(ParkState::Waiting as usize),
                token: Cell::new(0),
                waker: Cell::new(None),
                prev: Cell::new(None),
                next: Cell::new(None),
                tail: Cell::new(None),
            },
            parker,
            wait_fn: Cell::new(Some(wait_fn)),
            cancel_fn: Cell::new(Some(cancel_fn)),
        }
    }
}

impl<'a, Event, WaitFn, CancelFn> Drop for ParkFuture<'a, Event, WaitFn, CancelFn>
where
    Event: AutoResetEvent,
    WaitFn: FnOnce(&Event) -> Poll<bool>,
    CancelFn: FnOnce(usize, bool),
{
    fn drop(&mut self) {
        let state = self.node.state.load(Ordering::Relaxed);
        if ParkState::from(state) == ParkState::Waiting {
            self.parker.queue.locked(&self.node.event, |head| unsafe {
                if let Ok(was_last) = Parker::remove(head, &self.node) {
                    if let Some(cancel) = self.cancel_fn.replace(None) {
                        cancel(self.node.token.get(), was_last);
                    }
                }
            });
        }
    }
}

impl<'a, Event, WaitFn, CancelFn> Future for ParkFuture<'a, Event, WaitFn, CancelFn>
where
    Event: AutoResetEvent,
    WaitFn: FnOnce(&Event) -> Poll<bool>,
    CancelFn: FnOnce(usize, bool),
{
    type Output = ParkResult;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        let mut state = this.node.state.load(Ordering::Relaxed);

        loop {
            match ParkState::from(state) {
                ParkState::Waiting => match this.wait_fn.replace(None) {
                    Some(wait) => match wait(&this.node.event) {
                        Poll::Pending | Poll::Ready(true) => {
                            state = this.node.state.load(Ordering::Relaxed);
                        }
                        Poll::Ready(false) => {
                            this.parker.queue.locked(&this.node.event, |head| unsafe {
                                if let Ok(was_last) = Parker::remove(head, &this.node) {
                                    if let Some(cancel) = this.cancel_fn.replace(None) {
                                        cancel(this.node.token.get(), was_last);
                                    }
                                    state = ParkState::Cancelled as usize;
                                    this.node.state.store(state, Ordering::Relaxed);
                                } else {
                                    state = this.node.state.load(Ordering::Relaxed);
                                }
                            })
                        }
                    },
                    None => match this.node.state.compare_exchange_weak(
                        state,
                        ParkState::Updating as usize,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Err(e) => state = e,
                        Ok(_) => {
                            let new_waker = Some(ctx.waker().clone());
                            this.node.waker.replace(new_waker);

                            state = this.node.state.load(Ordering::Relaxed);
                            if ParkState::from(state) == ParkState::Updating {
                                state = this.node.state.compare_and_swap(
                                    ParkState::Updating as usize,
                                    ParkState::Waiting as usize,
                                    Ordering::Release,
                                );
                            }

                            if ParkState::from(state) == ParkState::Updating {
                                return Poll::Pending;
                            } else {
                                this.node.waker.replace(None);
                            }
                        }
                    },
                },
                ParkState::Updating => {
                    unreachable!("ParkFuture being polled in parallel");
                }
                ParkState::Unparked => {
                    let token = this.node.token.get();
                    return Poll::Ready(ParkResult::Unparked(token));
                }
                ParkState::Cancelled => {
                    let token = this.node.token.get();
                    return Poll::Ready(ParkResult::Cancelled(token));
                }
            }
        }
    }
}

struct UnparkList<Event> {
    overflow_head: Option<NonNull<ParkNode<Event>>>,
    overflow_tail: Option<NonNull<ParkNode<Event>>>,
    array_size: usize,
    array: [MaybeUninit<NonNull<ParkNode<Event>>>; 16],
}

impl<Event> UnparkList<Event> {
    pub fn new() -> Self {
        Self {
            overflow_head: None,
            overflow_tail: None,
            array_size: 0,
            array: unsafe { MaybeUninit::uninit().assume_init() },
        }
    }

    pub fn push(&mut self, node: NonNull<ParkNode<Event>>) {
        if self.array_size < self.array.len() {
            self.array[self.array_size] = MaybeUninit::new(node);
            self.array_size += 1;
            return;
        }

        unsafe {
            (&*node.as_ptr()).next.set(None);
            if let Some(tail) = self.overflow_tail {
                (&*tail.as_ptr()).next.set(Some(node));
            } else {
                self.overflow_head = Some(node);
                self.overflow_tail = Some(node);
            }
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = NonNull<ParkNode<Event>>> + '_ {
        struct NodeIter<'a, Event> {
            array_pos: usize,
            list: &'a UnparkList<Event>,
            overflow_node: Option<NonNull<ParkNode<Event>>>,
        }

        impl<'a, Event> Iterator for NodeIter<'a, Event> {
            type Item = NonNull<ParkNode<Event>>;

            fn next(&mut self) -> Option<Self::Item> {
                unsafe {
                    if self.array_pos < self.list.array_size {
                        let node = self.list.array[self.array_pos].assume_init();
                        self.array_pos += 1;
                        Some(node)
                    } else if let Some(node) = self.overflow_node {
                        self.overflow_node = (&*node.as_ptr()).next.get();
                        Some(node)
                    } else {
                        None
                    }
                }
            }
        }

        NodeIter {
            array_pos: 0,
            list: self,
            overflow_node: self.overflow_head,
        }
    }
}
