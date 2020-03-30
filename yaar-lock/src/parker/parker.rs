use super::{Lock, Node, AutoResetEvent};
use core::{
    pin::Pin,
    cell::Cell,
    mem::MaybeUninit,
    future::Future,
    marker::PhantomPinned,
    task::{Poll, Waker, Context},
    sync::atomic::{Ordering, AtomicUsize},
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
    Cancel,
}

#[derive(Debug)]
pub enum UnparkContext {
    has_more: bool,
    unparked: usize,
    cancelled: usize,
}

struct ParkContext<Event> {
    state: AtomicUsize,
    node: Node<Event>,
    token: Cell<usize>,
}

pub struct Parker<Event> {
    queue: Lock<Event, Option<NonNull<ParkContext<Event>>>>,
}

impl<Event> Parker<Event> {
    pub const fn new() -> Self {
        Self {
            queue: Lock::new(None),
        }
    }
}

impl<Event: AutoResetEvent> Parker<Event> {
    pub unsafe async fn park(
        &self,
        init_event: impl FnOnce() -> Event,
        wait: impl FnOnce(&Event) -> Poll<bool>,
        prepare: impl FnOnce() -> Result<usize, ()>,
        cancel: impl FnOnce(usize), 
    ) -> ParkResult {
        let future = ParkFuture::new();
        let node = &future.context.node;

        match self.queue.locked(
            &future.context.node,
            init_event,
            |head| prepare().map(|token| {
                future.context.token.set(token);
                node.prev.set(MaybeUninit::new(None));
                node.next.set(MaybeUninit::new(head));
                node.tail.set(MaybeUninit::new({
                    let node_ptr = NonNull::new(node as *const _ as *mut _);
                    if let Some(head) = head.map(|p| &*p.as_ptr()) {
                        head.prev.set(MaybeUninit::new(node_ptr));
                        head.tail.get().assume_init()
                    } else {
                        node_ptr
                    }
                });
            })
        ) {
            Err(_) => ParkResult::Unprepared,
            Ok(_) => match wait(node.event()) {
                Poll::Pending => {
                    future.cancel.set(Some(cancel));
                    future.await
                },
                Poll::Done(false) => {
                    let token = future.context.token.get();
                    cancel(token);
                    ParkResult::Cancelled(token)
                },
                Poll::Done(true) => {
                    let token = future.context.token.get();
                    let state = future.context.state.load(Ordering::Relaxed);
                    if ParkState::from(state) == ParkState::Cancelled {
                        ParkResult::Cancelled(token)
                    } else {
                        ParkResult::Unparked(token)
                    }
                },
            }
        }
    }

    pub unsafe fn unpark(
        &self,
        filter: impl FnMut(&UnparkContext, usize) -> UnparkResult,
        notify: impl FnMut(&Event, Waker),
    ) {
        let mut unpark_context = UnparkContext {
            has_more: true,
            unparked: 0,
            cancelled: 0,
        };

        self.queue.locked(|head| {
            
        })
    }
}

struct UnparkList<Event> {
    size: usize,
    overflow_head: Option<NonNull<ParkContext<Event>>>,
    overflow_tail: Option<NonNull<ParkContext<Event>>>,
    array: [MaybeUninit<NonNull<ParkContext<Event>>>; 32],
}

impl<Event> UnparkList<Event> {
    pub const fn new() -> Self {
        Self {
            size: 0,
            overflow_head: None,
            overflow_tail: None,
            array: 
        }
    }

    pub fn push(&mut self, node: NonNull<ParkContext<Event>>) {
        unsafe {
            if self.size < self.array.len() {
                self.array[self.size] = MaybeUninit::new(node);
                self.size += 1;
                return;
            }
            
            (&*node.as_ptr()).next(MaybeUninit::new(None));
            if let Some(tail) = self.overflow_tail {
                (&*tail.as_ptr()).next.set(MaybeUninit::new(node));
            }
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = NonNull<ParkContext<Event>>> {

    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
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

struct ParkFuture<'a, Event, CancelFn> {
    parker: &'a Parker<Event>,
    context: ParkContext<Event>,
    cancel: Option<CancelFn>,
}

impl<'a, Event, CancelFn> Drop for ParkFuture<'a, Event, CancelFn>
where
    CancelFn: impl FnOnce(usize),
{
    fn drop(&mut self) {
        let state = self.context.state.load(Ordering::Relaxed);
        if ParkState::from(state) != ParkState::Waiting {
            return;
        }

        self.parker.queue.locked(|head| {
            let state = self.context.state.load(Ordering::Relaxed);
            if ParkState::from(state) != ParkState::Waiting {
                return;
            }

            let token = self.context.token.get();
            Parker::<Event>::remove(head, &self.context.node);
            self.cancel.replace(None).unwrap()(token);
        })
    }
}

impl<'a, Event, CancelFn> Future for ParkFuture<'a, Event, CancelFn>
where
    CancelFn: impl FnOnce(usize),
{
    type Output = ParkResult;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        unsafe {
            let this = self.get_unchecked();
            let mut state = this.context.state.load(Ordering::Relaxed);
            
            loop {
                match ParkState::from(state) {
                    ParkState::Waiting => return Poll::Pending,
                    ParkState::Updating => unreachable!(),
                    ParkState::Notified => {
                        return Poll::Ready()
                    },
                }
            }
        }
    }
}