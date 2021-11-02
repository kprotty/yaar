use super::{
    driver::Driver,
    wakers::{WakerIndex, WakerKind},
};
use crate::runtime::scheduler::Thread;
use mio::event::Source;
use std::{
    future::Future,
    io, mem,
    pin::Pin,
    sync::atomic::{AtomicU8, Ordering},
    sync::Arc,
    task::{Context, Poll, Waker},
};

const BUDGET: u8 = 128;

pub struct Pollable<S: Source> {
    budgets: [AtomicU8; 2],
    driver: Arc<Driver>,
    index: WakerIndex,
    source: S,
}

impl<S: Source> Pollable<S> {
    pub fn new(mut source: S) -> io::Result<Self> {
        Thread::try_with(|thread| {
            let driver = &thread.executor.io_driver;
            let index = match driver.wakers.alloc() {
                Some(index) => index,
                None => return Err(io::Error::from(io::ErrorKind::OutOfMemory)),
            };

            if let Err(error) = driver.registry.register(
                &mut source,
                mio::Token(index.into()),
                mio::Interest::READABLE | mio::Interest::WRITABLE,
            ) {
                driver.wakers.free(index);
                return Err(error);
            }

            Ok(Self {
                budgets: [AtomicU8::new(BUDGET), AtomicU8::new(BUDGET)],
                driver: driver.clone(),
                index,
                source,
            })
        })
        .expect("Using I/O primitives outside of the runtime context")
    }
}

impl<S: Source> Drop for Pollable<S> {
    fn drop(&mut self) {
        self.driver
            .registry
            .deregister(&mut self.source)
            .expect("I/O source failed to deregister from I/O selector");

        let resumed = self.driver.wakers.with(self.index, |wakers| {
            [WakerKind::Read, WakerKind::Write]
                .into_iter()
                .filter_map(|kind| wakers[kind as usize].wake())
                .map(mem::drop)
                .count()
        });

        if resumed > 0 {
            let pending = self.driver.pending.fetch_sub(resumed, Ordering::Relaxed);
            assert!(pending >= resumed);
        }

        self.driver.wakers.free(self.index);
    }
}

impl<S: Source> AsRef<S> for Pollable<S> {
    fn as_ref(&self) -> &S {
        &self.source
    }
}

impl<S: Source> Pollable<S> {
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
        waker: Option<&Waker>,
        mut do_io: impl FnMut() -> io::Result<T>,
    ) -> Poll<io::Result<T>> {
        self.driver.wakers.with(self.index, |wakers| loop {
            match wakers[kind as usize].poll(
                waker,
                || {
                    let pending = self.driver.pending.fetch_add(1, Ordering::Relaxed);
                    assert_ne!(pending, usize::MAX);
                },
                || {
                    let pending = self.driver.pending.fetch_sub(1, Ordering::Relaxed);
                    assert_ne!(pending, 0);
                },
            ) {
                Poll::Ready(_) => {}
                Poll::Pending => return Poll::Pending,
            }

            if let Some(waker) = waker {
                let budget = &self.budgets[kind as usize];
                let new_budget = budget.load(Ordering::Relaxed).checked_sub(1);
                budget.store(new_budget.unwrap_or(BUDGET), Ordering::Relaxed);

                if new_budget.is_none() {
                    waker.wake_by_ref();
                    return Poll::Pending;
                }
            }

            loop {
                match do_io() {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        wakers[kind as usize].reset();
                        break;
                    }
                    result => return Poll::Ready(result),
                }
            }
        })
    }

    pub fn poll_future<'a, T: 'a>(
        &'a self,
        kind: WakerKind,
        do_io: impl FnMut() -> io::Result<T> + Unpin + 'a,
    ) -> impl Future<Output = io::Result<T>> + 'a {
        struct PollFuture<'a, S: Source, T, F: FnMut() -> io::Result<T> + Unpin + 'a> {
            do_io: F,
            kind: WakerKind,
            pollable: Option<&'a Pollable<S>>,
        }

        impl<'a, S: Source, T, F: FnMut() -> io::Result<T> + Unpin + 'a> Future
            for PollFuture<'a, S, T, F>
        {
            type Output = io::Result<T>;

            fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
                let pollable = self
                    .pollable
                    .take()
                    .expect("PollFuture polled after completion");

                match pollable.poll_io(self.kind, Some(ctx.waker()), &mut self.do_io) {
                    Poll::Ready(result) => Poll::Ready(result),
                    Poll::Pending => {
                        self.pollable = Some(pollable);
                        Poll::Pending
                    }
                }
            }
        }

        PollFuture {
            do_io,
            kind,
            pollable: Some(self),
        }
    }
}
