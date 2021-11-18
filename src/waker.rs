use std::{
    mem::replace,
    task::{Waker, Poll, Context},
    sync::Mutex,
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
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
    pub fn poll(&self, ctx: Option<&mut Context<'_>>) -> Poll<()> {
        if self.notified.load(Ordering::Relaxed) {
            return Poll::Ready(());
        }

        let poll_result = (|| {
            let state = self.state.load(Ordering::Acquire); // synchronize with wake()
            match state {
                EMPTY | READY => {},
                NOTIFIED => return Poll::Ready(()),
                UPDATING => unreachable!("multiple threads polling the same AtomicWaker"),
                _ => unreachable!("invalid AtomicWaker state"),
            }

            let ctx = match ctx {
                Some(ctx) => ctx,
                None => return Poll::Pending,
            };

            if let Err(state) = self.state.compare_exchange(
                state,
                UPDATING,
                Ordering::Acquire, // keep waker updating before we transition to UPDATING
                Ordering::Acquire, // synchronize with wake()
            ) {
                assert_eq!(state, NOTIFIED);
                return Poll::Ready(());
            }

            // NOTE: Mutex is needed instead of TryLock as this could contend with wake():
            // - wake() updates to NOTIFIED, gets preempted
            // - we reset(), then call poll() and update the waker
            // - wake() is rescheduled and also tries to consume the waker
            {
                let mut waker = self.waker.lock().unwrap();
                let will_wake = waker
                    .as_ref()
                    .map(|waker_ref| ctx.waker().will_wake(waker_ref))
                    .unwrap_or(false);

                if !will_wake {
                    *waker = Some(ctx.waker().clone());
                }
            }

            match self.state.compare_exchange(
                UPDATING,
                READY,
                Ordering::AcqRel, // keep waker updating before transition to READY
                Ordering::Acquire, // synchronize with wake()
            ) {
                Ok(_) => Poll::Pending,
                Err(NOTIFIED) => Poll::Ready(()),
                Err(_) => unreachable!("invalid AtomicWaker state"),
            }
        })();

        if poll_result.is_ready() {
            self.state.store(EMPTY, Ordering::Relaxed);
            self.notified.store(true, Ordering::Relaxed);
        }

        poll_result
    }

    pub fn reset(&self) {
        self.notified.store(false, Ordering::Relaxed);
    }

    pub fn wake(&self) {
        self.state
            .fetch_update(
                Ordering::Release, // synchronize with poll()
                Ordering::Relaxed,
                |state| match state {
                    NOTIFIED => None,
                    EMPTY | READY | UPDATING => Some(NOTIFIED),
                    _ => unreachable!("invalid AtomicWaker state"),
                },
            )
            .ok()
            .and_then(|state| match state {
                READY => replace(&mut *self.waker.lock().unwrap(), None),
                _ => None,
            })
            .map(Waker::wake)
            .unwrap_or(())
    }
}