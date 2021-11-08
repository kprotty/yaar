use super::{
    driver::Driver,
    waker::{WakerIndex, WakerKind},
};
use mio::event::Source;
use std::{
    future::Future,
    hint::spin_loop,
    io,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
    sync::Arc,
    task::{Context, Poll},
};

const BUDGET: u8 = 128;

pub struct Pollable<S: Source> {
    source: S,
    index: WakerIndex,
    driver: Arc<Driver>,
    budgets: [AtomicU8; 2],
    pendings: [AtomicBool; 2],
}

impl<S: Source> Pollable<S> {
    pub fn new(mut source: S) -> io::Result<Self> {
        Driver::with(|driver| {
            let index = driver.register(&mut source)?;
            Ok(Self {
                source,
                index,
                driver: driver.clone(),
                budgets: [AtomicU8::new(BUDGET), AtomicU8::new(BUDGET)],
                pendings: [AtomicBool::new(false), AtomicBool::new(false)],
            })
        })
    }

    pub fn try_io<T>(
        &self,
        kind: WakerKind,
        do_io: impl FnMut() -> io::Result<T>,
    ) -> io::Result<T> {
        match self.poll_io(kind, None, do_io) {
            Poll::Ready(result) => result,
            Poll::Pending => Err(io::Error::from(io::ErrorKind::WouldBlock)),
        }
    }

    pub fn poll_io<T>(
        &self,
        kind: WakerKind,
        mut ctx: Option<&mut Context<'_>>,
        mut do_io: impl FnMut() -> io::Result<T>,
    ) -> Poll<io::Result<T>> {
        self.driver.with_wakers(self.index, |wakers| loop {
            let pending = &self.pendings[kind as usize];
            let is_pending = pending.load(Ordering::Relaxed);

            if wakers[kind as usize].poll(ctx.as_deref_mut()).is_pending() {
                if !is_pending {
                    self.driver.io_pending_begin();
                    pending.store(true, Ordering::Relaxed);
                }
                return Poll::Pending;
            }

            if is_pending {
                self.driver.io_pending_complete();
                pending.store(false, Ordering::Relaxed);
            }

            if let Some(ctx) = ctx.as_ref() {
                let budget = &self.budgets[kind as usize];
                let new_budget = budget.load(Ordering::Relaxed).checked_sub(1);
                budget.store(new_budget.unwrap_or(BUDGET), Ordering::Relaxed);

                if new_budget.is_none() {
                    ctx.waker().wake_by_ref();
                    return Poll::Pending;
                }
            }

            loop {
                match do_io() {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => spin_loop(),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        wakers[kind as usize].reset();
                        break;
                    }
                    result => return Poll::Ready(result),
                }
            }
        })
    }

    pub async fn poll_future<T>(
        &self,
        kind: WakerKind,
        do_io: impl FnMut() -> io::Result<T> + Unpin,
    ) -> io::Result<T> {
        struct PollFuture<'a, S: Source, T, F: FnMut() -> io::Result<T> + Unpin> {
            pollable: Option<&'a Pollable<S>>,
            kind: WakerKind,
            do_io: F,
        }

        impl<'a, S: Source, T, F: FnMut() -> io::Result<T> + Unpin> Future for PollFuture<'a, S, T, F> {
            type Output = io::Result<T>;

            fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
                let kind = self.kind;
                let pollable = self
                    .pollable
                    .take()
                    .expect("PollFuture polled after completion");

                if let Poll::Ready(result) = pollable.poll_io(kind, Some(ctx), &mut self.do_io) {
                    return Poll::Ready(result);
                }

                self.pollable = Some(pollable);
                Poll::Pending
            }
        }

        PollFuture {
            pollable: Some(self),
            kind,
            do_io,
        }
        .await
    }
}

impl<S: Source> AsRef<S> for Pollable<S> {
    fn as_ref(&self) -> &S {
        &self.source
    }
}

impl<S: Source> Drop for Pollable<S> {
    fn drop(&mut self) {
        self.driver.with_wakers(self.index, |wakers| {
            wakers[WakerKind::Read as usize].wake();
            wakers[WakerKind::Write as usize].wake();
        });

        self.driver.deregister(&mut self.source, self.index);
    }
}
