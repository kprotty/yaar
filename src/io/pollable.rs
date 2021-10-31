use super::{WakerIndex, WakerKind};
use crate::{runtime::scheduler::Thread, sync::AtomicWaker};
use mio::event::Source;
use std::{
    future::Future,
    io, mem,
    pin::Pin,
    sync::atomic::Ordering,
    sync::Arc,
    task::{Context, Poll, Waker},
};
use try_lock::TryLock;

pub struct Pollable<S: Source> {
    driver: Arc<Driver>,
    index: WakerIndex,
    source: S,
}

impl<S: Source> AsRef<S> for Pollable<S> {
    fn as_ref(&self) -> &S {
        &self.source
    }
}

impl<S: Source> Pollable<S> {
    pub fn new(source: S) -> io::Result<Self> {
        Thread::with_current(|thread| {
            let driver = &thread.executor.io_driver;
            Self::with_driver(source, driver)
        })
        .expect("Using I/O primitives outside of the runtime")
    }

    fn with_driver(mut source: S, driver: &Arc<Driver>) -> io::Result<Self> {
        let index = match driver.wakers.alloc() {
            Some(index) => index,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "Out of I/O Waker indices",
                ))
            }
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
            driver: driver.clone(),
            index,
            source,
        })
    }

    fn with_waker<F>(&self, kind: WakerKind, f: impl FnOnce(&AtomicWaker) -> F) -> F {
        self.driver
            .wakers
            .with(self.index, |wakers| f(&wakers[kind as usize]))
    }
}

impl<S: Source> Drop for Pollable<S> {
    fn drop(&mut self) {
        self.driver
            .registry
            .deregister(&mut self.source)
            .expect("I/O source failed to deregister from I/O selector");

        self.poll_detach(WakerKind::Read);
        self.poll_detach(WakerKind::Write);

        self.driver.wakers.free(self.index);
    }
}

impl<S: Source> Pollable<S> {
    fn poll_pending(&self) {
        let pending = self.driver.pending.fetch_add(1, Ordering::Relaxed);
        assert_ne!(pending, usize::MAX);
    }

    fn poll_cancel(&self) {
        let pending = self.driver.pending.fetch_sub(1, Ordering::Relaxed);
        assert_ne!(pending, 0);
    }

    fn poll_detach(&self, kind: WakerKind) {
        if let Some(waker) = self.with_waker(kind, |waker| waker.detach()) {
            self.poll_cancel();
            mem::drop(waker);
        }
    }

    pub fn poll_io<T>(
        &self,
        kind: WakerKind,
        waker_ref: &Waker,
        mut do_io: impl FnMut() -> io::Result<T>,
    ) -> Poll<io::Result<T>> {
        loop {
            let mut wake_token = match self.with_waker(kind, |waker| {
                waker.poll(waker_ref, || self.poll_pending(), || self.poll_cancel())
            }) {
                Poll::Ready(token) => token,
                Poll::Pending => return Poll::Pending,
            };

            loop {
                match do_io() {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        wake_token = match self.with_waker(kind, |waker| waker.reset(wake_token)) {
                            Err(token) => token,
                            Ok(_) => break,
                        };
                    }
                    result => return Poll::Ready(result),
                }
            }
        }
    }

    pub fn poll_future<'a, T: 'a>(
        &'a self,
        kind: WakerKind,
        do_io: impl FnMut() -> io::Result<T> + Unpin + 'a,
    ) -> impl Future<Output = io::Result<T>> + 'a {
        struct PollFuture<'a, S: Source, T, F: FnMut() -> io::Result<T> + Unpin> {
            do_io: F,
            completed: bool,
            kind: WakerKind,
            pollable: &'a Pollable<S>,
        }

        impl<'a, S: Source, T, F: FnMut() -> io::Result<T> + Unpin> Future for PollFuture<'a, S, T, F> {
            type Output = io::Result<T>;

            fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
                if self.completed {
                    unreachable!("PollFuture polled after completion");
                }

                let kind = self.kind;
                let pollable = self.pollable;
                let result = match pollable.poll_io(kind, ctx.waker(), &mut self.do_io) {
                    Poll::Ready(result) => result,
                    Poll::Pending => return Poll::Pending,
                };

                self.completed = true;
                Poll::Ready(result)
            }
        }

        impl<'a, S: Source, T, F: FnMut() -> io::Result<T> + Unpin> Drop for PollFuture<'a, S, T, F> {
            fn drop(&mut self) {
                if !self.completed {
                    self.pollable.poll_detach(self.kind);
                }
            }
        }

        PollFuture {
            do_io,
            completed: false,
            kind,
            pollable: self,
        }
    }
}

