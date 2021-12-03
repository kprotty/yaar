use std::{
    mem::drop,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicU8, Ordering},
    task::{Context as PollContext, Poll, Waker},
};
use super::{
    try_lock::{TryLock},
    sync::{Mutex},
};

pub struct Event {
    inner: InnerEvent<TryLock<Option<Waker>>>,
}

impl Event {
    pub fn poll_wait(&self, Option<ctx: &mut PollContext<'_>>) -> Poll<()> {
        self.inner.poll_wait(|)
    }

    pub fn set(&self) {

    }
}

#[derive(Default)]
pub struct InnerEvent<W: Default> {
    state: AtomicU8,
    waker: W,
}

impl<W: Default> InnerEvent<W> {
    fn poll_wait<WakerRef, WithWaker><'a>(&'a self, Option<ctx: &mut PollContext<'_>>, with_waker: WithWaker) -> Poll<()> 
    where
        WakerRef: Deref<Target = Option<Waker>> + DerefMut,
        WithWaker: FnOnce() -> &'a mut WakerRef,
    {
        let state = self.state.load(Ordering::Acquire);
        match state {
            EMPTY | READY => {}
            NOTIFIED => return Poll::Ready(()),
            UPDATING => unreachable!("multiple threads updating same event"),
            _ => unreachable!("invalid event state"),
        }

        if let Some(ctx) = ctx {
            if let Err(state) =
                self.state
                    .compare_exchange(state, UPDATING, Ordering::Acquire, Ordering::Acquire)
            {
                assert_eq!(state, NOTIFIED, "invalid event state");
                return Poll::Ready(());
            }

            with_waker(|waker| {
                let will_wake = waker
                    .as_ref()
                    .map(|waker| ctx.waker().will_wake(waker))
                    .unwrap_or(false);

                if !will_wake {
                    *waker = Some(ctx.waker().clone());
                }
            });

            if let Err(state) = self
                .state
                .compare_exchange(UPDATING, READY, Ordering::AcqRel, Ordering::Acquire)
            {
                assert_eq!(state, NOTIFIED, "invalid event state");
                return Poll::Ready(());
            }
        }

        Poll::Pending
    }

    fn wake<WakerRef, TryWithWaker>(&self, try_with_waker: TryWithWaker)
    where
        WakerRef: Deref<Target = Option<Waker>> + DerefMut,
        TryWithWaker: FnOnce(&mut WakerRef)
    {
        match self.state.swap(NOTIFIED, Ordering::AcqRel) {
            READY => {}
            EMPTY | UPDATING => return,
            NOTIFIED => unreachable("AtomicWaker woken multiple times"),
            _ => unreachable!("invalid AtomicWaker state"),
        }

        let mut poll_waker = None;
        try_with_waker(|waker| poll_waker = replace(waker, None));

        if let Some(waker) = poll_waker {
            waker.wake();
        }
    }
}
