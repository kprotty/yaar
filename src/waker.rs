use std::{
    mem::replace,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
    sync::Mutex,
    task::{Context, Poll, Waker},
};
use try_lock::TryLock;

#[derive(Default)]
pub struct ResetWaker {
    notified: AtomicBool,
    state: AtomicWakerState,
    waker: Mutex<Option<Waker>>,
}

impl ResetWaker {
    pub fn poll(&self, ctx: &mut Context<'_>) -> Poll<()> {
        if self.notified.load(Ordering::Relaxed) {
            return Poll::Ready(());
        }

        let poll_result = self.state.poll(ctx, || self.waker.lock().unwrap());
        if let Poll::Ready(_) = poll_result {
            self.notified.store(true, Ordering::Relaxed);
            self.state.reset();
        }

        poll_result
    }

    pub fn reset(&self) {
        assert!(self.notified.load(Ordering::Relaxed));
        self.notified.store(false, Ordering::Relaxed);
    }

    pub fn wake(&self) -> Option<Waker> {
        if self.state.wake() {
            if let Ok(mut waker) = self.waker.try_lock() {
                if let Some(waker) = replace(&mut *waker, None) {
                    return Some(waker);
                }
            }
        }

        None
    }
}

#[derive(Default)]
pub struct OneshotWaker {
    state: AtomicWakerState,
    waker: TryLock<Option<Waker>>,
}

impl OneshotWaker {
    pub fn poll(&self, ctx: &mut Context<'_>) -> Poll<()> {
        self.state.poll(ctx, || self.waker.try_lock().unwrap())
    }

    pub fn wake(&self) {
        if self.state.wake() {
            if let Some(waker) = replace(&mut *self.waker.try_lock().unwrap(), None) {
                waker.wake();
            }
        }
    }
}

#[derive(Default)]
struct AtomicWakerState {
    state: AtomicU8,
}

impl AtomicWakerState {
    const EMPTY: u8 = 0;
    const UPDATING: u8 = 1;
    const READY: u8 = 2;
    const NOTIFIED: u8 = 3;

    fn poll<W: Deref<Target = Option<Waker>> + DerefMut>(
        &self,
        ctx: &mut Context<'_>,
        lock_waker: impl FnOnce() -> W,
    ) -> Poll<()> {
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
            let mut waker = lock_waker();
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

    fn reset(&self) {
        self.state.store(Self::EMPTY, Ordering::Relaxed);
    }

    fn wake(&self) -> bool {
        match self.state.swap(Self::NOTIFIED, Ordering::AcqRel) {
            Self::READY => true,
            Self::EMPTY | Self::UPDATING | Self::NOTIFIED => false,
            _ => unreachable!("invalid AtomicWaker state"),
        }
    }
}
