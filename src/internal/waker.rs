use parking_lot::Mutex;
use std::{
    mem::replace,
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
    task::{Context, Poll, Waker},
};

const EMPTY: u8 = 0;
const UPDATING: u8 = 1;
const WAITING: u8 = 2;
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

        let poll_result = (|| {
            let state = self.state.load(Ordering::Acquire);
            match state {
                EMPTY | WAITING => {}
                NOTIFIED => return Poll::Ready(()),
                UPDATING => unreachable!("AtomicWaker being polled by multiple threads"),
                _ => unreachable!("AtomicWaker being polled with an invalid state"),
            }

            if let Some(ctx) = ctx {
                if let Err(state) = self.state.compare_exchange(
                    state,
                    UPDATING,
                    Ordering::Acquire,
                    Ordering::Acquire,
                ) {
                    assert_eq!(state, NOTIFIED);
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

                if let Err(state) = self.state.compare_exchange(
                    UPDATING,
                    WAITING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    assert_eq!(state, NOTIFIED);
                    return Poll::Ready(());
                }
            }

            Poll::Pending
        })();

        if poll_result.is_ready() {
            self.notified.store(true, Ordering::Relaxed);
            self.state.store(EMPTY, Ordering::Relaxed);
        }

        poll_result
    }

    #[cold]
    pub fn wake(&self) {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |state| match state {
                NOTIFIED => None,
                EMPTY | UPDATING | WAITING => Some(NOTIFIED),
                _ => unreachable!("AtomicWaker waking with invalid state"),
            })
            .ok()
            .and_then(|state| match state {
                WAITING => replace(&mut *self.waker.lock(), None),
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
