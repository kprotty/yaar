use super::wakers::{WakerIndex, WakerKind, Wakers};
use crate::{runtime::thread::Thread, sync::waker::AtomicWaker};
use mio::event::Source;
use std::{
    future::Future,
    io, mem,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    sync::Arc,
    task::{Context, Poll, Waker},
    time::Duration,
};
use try_lock::TryLock;

pub struct Driver {
    pending: AtomicUsize,
    selector: TryLock<mio::Poll>,
    registry: mio::Registry,
    signal: mio::Waker,
    wakers: Wakers,
}

impl Default for Driver {
    fn default() -> Self {
        let selector = mio::Poll::new().expect("Failed to create I/O selector");
        let registry = selector
            .registry()
            .try_clone()
            .expect("Failed to create I/O registry");

        let signal = mio::Waker::new(&registry, mio::Token(usize::MAX))
            .expect("Failed to create I/O signal");

        Self {
            pending: AtomicUsize::new(0),
            selector: TryLock::new(selector),
            registry,
            signal,
            wakers: Wakers::default(),
        }
    }
}

impl Driver {
    pub fn notify(&self) {
        self.signal.wake().expect("Failed to send an I/O signal");
    }
}

pub struct Poller {
    events: mio::event::Events,
}

impl Default for Poller {
    fn default() -> Self {
        Self {
            events: mio::event::Events::with_capacity(256),
        }
    }
}

impl Poller {
    pub fn poll(&mut self, driver: &Driver, timeout: Option<Duration>) -> bool {
        if driver.pending.load(Ordering::Relaxed) == 0 {
            return false;
        }

        let mut selector = match driver.selector.try_lock() {
            Some(guard) => guard,
            None => return false,
        };

        if driver.pending.load(Ordering::Relaxed) == 0 {
            return false;
        }

        let _ = selector.poll(&mut self.events, timeout);
        mem::drop(selector);

        let mut resumed = 0;
        for event in self.events.iter() {
            let index = match event.token() {
                mio::Token(usize::MAX) => continue,
                mio::Token(i) => WakerIndex::from(i),
            };

            let wakers = driver.wakers.with(index, |wakers| {
                [
                    match event.is_readable() {
                        true => wakers[WakerKind::Read as usize].wake(),
                        _ => None,
                    },
                    match event.is_writable() {
                        true => wakers[WakerKind::Write as usize].wake(),
                        _ => None,
                    },
                ]
            });

            wakers
                .into_iter()
                .filter_map(|waker| waker)
                .for_each(|waker| {
                    resumed += 1;
                    waker.wake();
                });
        }

        if resumed > 0 {
            let pending = driver.pending.fetch_sub(resumed, Ordering::Relaxed);
            assert!(pending >= resumed);
        }

        true
    }
}

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

        let notified = [WakerKind::Read, WakerKind::Write]
            .into_iter()
            .map(|kind| self.with_waker(kind, |waker| waker.wake()))
            .map(|waker| waker.is_some() as usize)
            .sum();

        if notified > 0 {
            let pending = self.driver.pending.fetch_sub(notified, Ordering::Relaxed);
            assert!(pending >= notified);
        }

        self.driver.wakers.free(self.index);
    }
}

impl<S: Source> Pollable<S> {
    pub fn poll_io<T>(
        &self,
        kind: WakerKind,
        waker_ref: &Waker,
        mut do_io: impl FnMut() -> io::Result<T>,
    ) -> Poll<io::Result<T>> {
        loop {
            let mut wake_token = match self.with_waker(kind, |waker| {
                waker.poll(waker_ref, || {
                    let pending = self.driver.pending.fetch_add(1, Ordering::Relaxed);
                    assert_ne!(pending, usize::MAX);
                })
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

        PollFuture {
            do_io,
            completed: false,
            kind,
            pollable: self,
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
struct FairState {
    tick: u8,
    be_fair: bool,
}

impl Into<u8> for FairState {
    fn into(self) -> u8 {
        (self.tick << 1) | (self.be_fair as u8)
    }
}

impl From<u8> for FairState {
    fn from(value: u8) -> Self {
        Self {
            tick: value >> 1,
            be_fair: value & 1 != 0,
        }
    }
}

#[derive(Default)]
pub struct PollFairness {
    state: u8,
}

impl PollFairness {
    pub fn poll_fair<T>(
        &mut self,
        waker_ref: &Waker,
        do_poll: impl FnOnce() -> Poll<T>,
    ) -> Poll<T> {
        let mut state: FairState = self.state.into();

        if state.be_fair {
            state.be_fair = false;
            self.state = state.into();

            waker_ref.wake_by_ref();
            return Poll::Pending;
        }

        let result = match do_poll() {
            Poll::Ready(result) => result,
            Poll::Pending => return Poll::Pending,
        };

        state.tick += 1;
        state.be_fair = state.tick == 0;
        self.state = state.into();

        Poll::Ready(result)
    }
}
