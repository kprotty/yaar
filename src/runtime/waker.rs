use crate::dependencies::try_lock::TryLock;
use std::{
    mem::replace,
    sync::atomic::{AtomicU8, Ordering},
    task::{Context as PollContext, Poll, Waker},
};

const EMPTY: u8 = 0;
const UPDATING: u8 = 1;
const READY: u8 = 2;
const NOTIFIED: u8 = 3;

#[derive(Default)]
pub struct AtomicWaker {
    state: AtomicU8,
    waker: TryLock<Option<Waker>>,
}

impl AtomicWaker {
    pub fn poll(&self, ctx: &mut PollContext<'_>) -> Poll<()> {
        let state = self.state.load(Ordering::Acquire);
        match state {
            EMPTY | READY => {}
            NOTIFIED => return Poll::Ready(()),
            UPDATING => unreachable!("multiple threads updating same AtomicWaker"),
            _ => unreachable!("invalid AtomicWaker state"),
        }

        if let Err(state) =
            self.state
                .compare_exchange(state, UPDATING, Ordering::Acquire, Ordering::Acquire)
        {
            assert_eq!(state, NOTIFIED);
            return Poll::Ready(());
        }

        {
            let mut waker = self.waker.try_lock().unwrap();
            let will_wake = waker
                .as_ref()
                .map(|w| ctx.waker().will_wake(w))
                .unwrap_or(false);

            if !will_wake {
                *waker = Some(ctx.waker().clone());
            }
        }

        match self
            .state
            .compare_exchange(UPDATING, READY, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Poll::Pending,
            Err(NOTIFIED) => Poll::Ready(()),
            Err(_) => unreachable!("invalid AtomicWaker state"),
        }
    }

    pub fn wake(&self) {
        match self.state.swap(NOTIFIED, Ordering::AcqRel) {
            READY => {}
            EMPTY | UPDATING => return,
            NOTIFIED => unreachable!("AtomicWaker woken multiple times"),
            _ => unreachable!("invalid AtomicWaker state"),
        }

        if let Some(waker) = {
            let mut waker = self.waker.try_lock().unwrap();
            replace(&mut *waker, None)
        } {
            waker.wake();
        }
    }
}
