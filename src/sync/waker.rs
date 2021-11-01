use parking_lot::Mutex;
use std::{
    mem,
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
    task::{Context, Poll, Waker},
};

const WAKER_EMPTY: u8 = 0;
const WAKER_UPDATING: u8 = 1;
const WAKER_READY: u8 = 2;
const WAKER_NOTIFIED: u8 = 3;

#[derive(Default)]
pub struct AtomicWaker {
    state: AtomicU8,
    notified: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl AtomicWaker {
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(0),
            notified: AtomicBool::new(false),
            waker: parking_lot::const_mutex(None),
        }
    }

    fn poll_update(&self, poll_fn: impl FnOnce() -> Poll<()>) -> Poll<()> {
        if poll_fn().is_pending() {
            return Poll::Pending;
        }

        self.state.store(WAKER_EMPTY, Ordering::Relaxed);
        self.notified.store(true, Ordering::Relaxed);
        Poll::Ready(())
    }

    fn poll_with(&self, poll_fn: impl FnOnce() -> Poll<()>) -> Poll<()> {
        if self.notified.load(Ordering::Relaxed) {
            return Poll::Ready(());
        }

        self.poll_update(poll_fn)
    }

    pub fn reset(&self) -> bool {
        self.notified.store(false, Ordering::Relaxed);

        self.poll_update(|| match self.state.load(Ordering::Acquire) {
            WAKER_EMPTY => Poll::Pending,
            WAKER_NOTIFIED => Poll::Ready(()),
            WAKER_READY => unreachable!("AtomicWaker state was updated while notified"),
            WAKER_UPDATING => unreachable!("AtomicWaker state is updating while notified"),
            _ => unreachable!(),
        })
        .is_pending()
    }

    pub fn poll_ready(&self) -> Poll<()> {
        self.poll_with(|| match self.state.load(Ordering::Acquire) {
            WAKER_NOTIFIED => Poll::Ready(()),
            WAKER_EMPTY | WAKER_READY => Poll::Pending,
            WAKER_UPDATING => unreachable!("AtomicWaker state updated while polling"),
            _ => unreachable!(),
        })
    }

    pub fn poll(
        &self,
        ctx: &mut Context<'_>,
        waiting: impl FnOnce(),
        cancelled: impl FnOnce(),
    ) -> Poll<()> {
        self.poll_with(|| {
            let mut state = self.state.load(Ordering::Acquire);
            match state {
                WAKER_EMPTY | WAKER_READY => {}
                WAKER_NOTIFIED => return Poll::Ready(()),
                WAKER_UPDATING => unreachable!("AtomicWaker state updated while polling"),
                _ => unreachable!("invalid AtomicWaker state"),
            }

            match self.state.compare_exchange(
                state,
                WAKER_UPDATING,
                Ordering::Acquire,
                Ordering::Acquire,
            ) {
                Ok(_) => {}
                Err(WAKER_NOTIFIED) => return Poll::Ready(()),
                Err(_) => unreachable!("invalid AtomicWaker state"),
            }

            {
                let mut waker = self.waker.lock();
                let will_wake = waker
                    .as_ref()
                    .map(|waker| ctx.waker().will_wake(waker))
                    .unwrap_or(false);

                if !will_wake {
                    *waker = Some(ctx.waker().clone());
                }
            }

            if let Err(new_state) = self.state.compare_exchange(
                WAKER_UPDATING,
                WAKER_READY,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                *self.waker.lock() = None;
                match state {
                    WAKER_EMPTY => {}
                    WAKER_READY => cancelled(),
                    _ => unreachable!(),
                }

                assert_eq!(new_state, WAKER_NOTIFIED);
                return Poll::Ready(());
            }

            match state {
                WAKER_EMPTY => waiting(),
                WAKER_READY => {}
                _ => unreachable!(),
            }

            Poll::Pending
        })
    }

    pub fn detach(&self) -> Option<Waker> {
        self.notify(Ordering::Acquire, |state| match state {
            WAKER_READY => Some(WAKER_EMPTY),
            WAKER_EMPTY | WAKER_NOTIFIED => None,
            WAKER_UPDATING => unreachable!("AtomicWaker state updated while detaching"),
            _ => unreachable!(),
        })
    }

    pub fn wake(&self) -> Option<Waker> {
        self.notify(Ordering::AcqRel, |state| match state {
            WAKER_NOTIFIED => None,
            WAKER_EMPTY | WAKER_READY | WAKER_UPDATING => Some(WAKER_NOTIFIED),
            _ => unreachable!("invalid AtomicWaker state"),
        })
    }

    fn notify(&self, ordering: Ordering, update: impl FnMut(u8) -> Option<u8>) -> Option<Waker> {
        self.state
            .fetch_update(ordering, Ordering::Relaxed, update)
            .ok()
            .and_then(|state| match state {
                WAKER_READY => mem::replace(&mut *self.waker.lock(), None),
                _ => None,
            })
    }
}
