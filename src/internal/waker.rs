use parking_lot::Mutex;
use std::{
    mem::replace,
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
    task::{Context, Poll, Waker},
};

const EMPTY: u8 = 0;
const UPDATING: u8 = 1;
const READY: u8 = 2;
const NOTIFIED: u8 = 3;

#[derive(Default)]
pub struct AtomicWaker {
    state: AtomicU8,
    notified: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl AtomicWaker {
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(EMPTY),
            notified: AtomicBool::new(false),
            waker: parking_lot::const_mutex(None),
        }
    }

    pub fn poll(&self, ctx: Option<&mut Context<'_>>) -> Poll<()> {
        if self.notified.load(Ordering::Relaxed) {
            return Poll::Ready(());
        }

        let poll_result = self.poll_update(ctx);
        self.notified
            .store(poll_result.is_ready(), Ordering::Relaxed);
        poll_result
    }

    fn poll_update(&self, ctx: Option<&mut Context<'_>>) -> Poll<()> {
        let state = self.state.load(Ordering::Acquire);
        match state {
            NOTIFIED => return Poll::Ready(()),
            EMPTY | READY => {}
            _ => unreachable!(),
        }

        let ctx = match ctx {
            Some(ctx) => ctx,
            None => return Poll::Pending,
        };

        match self
            .state
            .compare_exchange(state, UPDATING, Ordering::Acquire, Ordering::Acquire)
        {
            Ok(_) => {}
            Err(NOTIFIED) => return Poll::Ready(()),
            Err(_) => unreachable!(),
        }

        let mut waker = self.waker.try_lock().unwrap();
        let will_wake = waker
            .as_ref()
            .map(|waker| ctx.waker().will_wake(waker))
            .unwrap_or(false);

        if !will_wake {
            *waker = Some(ctx.waker().clone());
        }

        drop(waker);
        match self
            .state
            .compare_exchange(UPDATING, READY, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Poll::Pending,
            Err(NOTIFIED) => Poll::Ready(()),
            _ => unreachable!(),
        }
    }

    #[cold]
    pub fn wake(&self) {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |state| match state {
                EMPTY | UPDATING | READY => Some(NOTIFIED),
                NOTIFIED => None,
                _ => unreachable!(),
            })
            .ok()
            .and_then(|state| match state {
                READY => replace(&mut *self.waker.lock(), None),
                _ => None,
            })
            .map(Waker::wake)
            .unwrap_or(())
    }

    pub fn reset(&self) {
        assert!(self.notified.load(Ordering::Relaxed));
        self.notified.store(false, Ordering::Relaxed);
    }
}
