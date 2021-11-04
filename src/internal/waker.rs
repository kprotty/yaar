use parking_lot::Mutex;
use std::{
    mem::replace,
    sync::atomic::{AtomicU8, Ordering},
    task::{Context, Poll, Waker},
};

const EMPTY: u8 = 0;
const UPDATING: u8 = 1;
const WAITING: u8 = 2;
const AWOKEN: u8 = 3;
const NOTIFIED: u8 = 4;

#[derive(Default)]
pub struct AtomicWaker {
    state: AtomicU8,
    waker: Mutex<Option<Waker>>,
}

impl AtomicWaker {
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(EMPTY),
            waker: parking_lot::const_mutex(None),
        }
    }

    pub fn poll(&self, ctx: Option<&mut Context<'_>>) -> Poll<()> {
        let state = self.state.load(Ordering::Acquire);
        if state & NOTIFIED != 0 {
            return Poll::Ready(());
        }

        match state & 0b11 {
            AWOKEN => {
                self.state.store(EMPTY | NOTIFIED, Ordering::Relaxed);
                return Poll::Ready(());
            }
            EMPTY | WAITING => {}
            UPDATING => unreachable!("AtomicWaker being polled by multiple threads"),
            _ => unreachable!("AtomicWaker being polled with an invalid state"),
        }

        if let Some(ctx) = ctx {
            if let Err(state) =
                self.state
                    .compare_exchange(state, UPDATING, Ordering::Acquire, Ordering::Acquire)
            {
                assert_eq!(state, AWOKEN | NOTIFIED);
                self.state.store(EMPTY | NOTIFIED, Ordering::Relaxed);
                return Poll::Ready(());
            }

            {
                let mut waker = self.waker.try_lock().unwrap();
                let will_wake = waker
                    .as_ref()
                    .map(|waker| ctx.waker().will_wake(waker))
                    .unwrap_or(false);

                if !will_wake {
                    *waker = Some(ctx.waker().clone());
                }
            }

            if let Err(state) =
                self.state
                    .compare_exchange(UPDATING, WAITING, Ordering::AcqRel, Ordering::Acquire)
            {
                assert_eq!(state, AWOKEN | NOTIFIED);
                *self.waker.try_lock().unwrap() = None;
                self.state.store(EMPTY | NOTIFIED, Ordering::Relaxed);
                return Poll::Ready(());
            }
        }

        Poll::Pending
    }

    #[cold]
    pub fn wake(&self) {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |state| {
                match state & 0b11 {
                    AWOKEN => None,
                    EMPTY | UPDATING | WAITING => Some(AWOKEN | NOTIFIED),
                    _ => unreachable!("AtomicWaker waking with invalid state"),
                }
            })
            .ok()
            .and_then(|state| match state & 0b11 {
                WAITING => replace(&mut *self.waker.lock(), None),
                _ => None,
            })
            .map(Waker::wake)
            .unwrap_or(())
    }

    pub fn reset(&self) {
        match self.state.compare_exchange(
            EMPTY | NOTIFIED,
            EMPTY,
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            Ok(_) => {}
            Err(AWOKEN | NOTIFIED) => {}
            Err(_) => unreachable!("AtomicWaker reset with invalid state"),
        }
    }
}
