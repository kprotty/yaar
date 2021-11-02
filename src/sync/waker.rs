use parking_lot::Mutex;
use std::{
    mem,
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
    task::{Poll, Waker},
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

    pub fn poll(
        &self,
        waker: Option<&Waker>,
        waiting: impl FnOnce(),
        cancelled: impl FnOnce(),
    ) -> Poll<()> {
        if self.notified.load(Ordering::Relaxed) {
            return Poll::Ready(());
        }

        let poll_result = (|| {
            let state = self.state.load(Ordering::Acquire);
            match state {
                WAKER_EMPTY | WAKER_READY => {}
                WAKER_NOTIFIED => return Poll::Ready(()),
                WAKER_UPDATING => unreachable!("AtomicWaker being polled by multiple threads"),
                _ => unreachable!("invalid AtomicWaker state"),
            }

            if let Some(waker_ref) = waker {
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
                        .map(|waker| waker_ref.will_wake(waker))
                        .unwrap_or(false);

                    if !will_wake {
                        *waker = Some(waker_ref.clone());
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
            }

            Poll::Pending
        })();

        if poll_result.is_ready() {
            self.state.store(WAKER_EMPTY, Ordering::Relaxed);
            self.notified.store(true, Ordering::Relaxed);
        }

        poll_result
    }

    pub fn reset(&self) {
        self.notified.store(false, Ordering::Relaxed);
    }

    pub fn wake(&self) -> Option<Waker> {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |state| match state {
                WAKER_NOTIFIED => None,
                WAKER_EMPTY | WAKER_READY | WAKER_UPDATING => Some(WAKER_NOTIFIED),
                _ => unreachable!("invalid AtomicWaker state"),
            })
            .ok()
            .and_then(|state| match state {
                WAKER_READY => mem::replace(&mut *self.waker.lock(), None),
                _ => None,
            })
    }
}
