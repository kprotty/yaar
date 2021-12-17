use std::{
    mem::replace,
    sync::atomic::{AtomicU8, Ordering},
    task::{Context, Poll, Waker},
    thread,
};
use try_lock::TryLock;

#[derive(Default)]
pub struct AtomicWaker {
    state: AtomicU8,
    waker: TryLock<Option<Waker>>,
}

impl AtomicWaker {
    const EMPTY: u8 = 0;
    const UPDATING: u8 = 1;
    const READY: u8 = 2;
    const NOTIFIED: u8 = 3;

    pub fn poll(&self, ctx: &mut Context<'_>) -> Poll<()> {
        let state = self.state.load(Ordering::Acquire);
        match state {
            Self::EMPTY | Self::READY => {}
            Self::NOTIFIED => return Poll::Ready(()),
            Self::UPDATING => unreachable!("AtomicWaker polled by multiple threads"),
            _ => unreachable!("invalid AtomicWaker state"),
        }

        if let Err(state) =
            self.state
                .compare_exchange(state, Self::UPDATING, Ordering::Acquire, Ordering::Acquire)
        {
            assert_eq!(state, Self::NOTIFIED);
            return Poll::Ready(());
        }

        {
            let mut waker = loop {
                match self.waker.try_lock() {
                    Some(guard) => break guard,
                    None => thread::yield_now(),
                }
            };

            let will_wake = waker
                .as_ref()
                .map(|w| ctx.waker().will_wake(w))
                .unwrap_or(false);

            if !will_wake {
                *waker = Some(ctx.waker().clone());
            }
        }

        match self.state.compare_exchange(
            Self::UPDATING,
            Self::READY,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Poll::Pending,
            Err(Self::NOTIFIED) => Poll::Ready(()),
            Err(_) => unreachable!("invalid AtomicWaker state"),
        }
    }

    pub fn reset(&self) {
        self.state.store(Self::EMPTY, Ordering::Relaxed);
    }

    pub fn wake(&self) -> Option<Waker> {
        match self.state.swap(Self::NOTIFIED, Ordering::AcqRel) {
            Self::READY => {}
            Self::EMPTY | Self::UPDATING | Self::NOTIFIED => return None,
            _ => unreachable!("invalid AtomicWaker state"),
        }

        let mut waker = self.waker.try_lock()?;
        replace(&mut *waker, None)
    }
}
