use super::wakers::{WakerIndex, WakerKind, Wakers};
use crate::{runtime::thread::Thread, sync::waker::AtomicWaker};
use mio::event::Source;
use std::{
    future::Future,
    io,
    mem,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    sync::Arc,
    task::{ready, Context, Poll, Waker},
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
    pub fn poll(&mut self, driver: &Driver, timeout: Option<Duration>) {
        if driver.pending.load(Ordering::Relaxed) == 0 {
            return;
        }

        let mut selector = match driver.selector.try_lock() {
            Some(guard) => guard,
            None => return,
        };

        let _ = selector.poll(&mut self.events, timeout);
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
        self.driver.wakers.with(self.index, kind, f)
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
            .map(|kind| self.with_waker(kind, |waker| waker.detach()))
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
    pub fn poll_ready(&self, kind: WakerKind, waker_ref: &Waker) -> Poll<u8> {
        self.with_waker(kind, |waker| {
            waker.poll(waker_ref, || {
                let pending = self.driver.pending.fetch_add(1, Ordering::Relaxed);
                assert_ne!(pending, usize::MAX);
            })
        })
    }

    pub fn try_io<T>(
        &self,
        kind: WakerKind,
        token: Option<u8>,
        mut do_io: impl FnMut() -> io::Result<T>,
    ) -> io::Result<T> {
        let mut waker_token = token;
        loop {
            match do_io() {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    let token = match waker_token {
                        Some(token) => token,
                        None => match self.with_waker(kind, |waker| waker.ready()) {
                            Some(token) => token,
                            None => return Err(e),
                        },
                    };

                    match self.with_waker(kind, |waker| waker.reset(token)) {
                        Err(token) => waker_token = Some(token),
                        Ok(_) => return Err(e),
                    }
                }
                result => return result,
            }
        }
    }

    pub fn poll_io<T>(
        &self,
        kind: WakerKind,
        waker_ref: &Waker,
        mut do_io: impl FnMut() -> io::Result<T>,
    ) -> Poll<io::Result<T>> {
        loop {
            let token = ready!(self.poll_ready(kind, waker_ref));

            match self.try_io(kind, Some(token), &mut do_io) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                result => return Poll::Ready(result),
            }
        }
    }

    pub fn poll_future<'a, T: 'a>(
        &'a self,
        kind: WakerKind,
        poll_fn: impl FnMut(&mut Context<'_>) -> Poll<T> + 'a + Unpin,
    ) -> impl Future<Output = T> + 'a {
        struct WaitFor<'a, S: Source, T, F: FnMut(&mut Context<'_>) -> Poll<T> + 'a + Unpin> {
            pollable: &'a Pollable<S>,
            poll_fn: Option<F>,
            kind: WakerKind,
        }

        impl<'a, S: Source, T, F: FnMut(&mut Context<'_>) -> Poll<T> + 'a + Unpin> Future for WaitFor<'a, S, T, F> {
            type Output = T;

            fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
                let poll_fn = self
                    .poll_fn
                    .as_mut()
                    .expect("poll_future() polled after completion");

                let result = ready!(poll_fn(ctx));
                mem::drop(poll_fn);
                self.poll_fn = None;
                Poll::Ready(result)
            }
        }

        impl<'a, S: Source, T, F: FnMut(&mut Context<'_>) -> Poll<T> + 'a + Unpin> Drop for WaitFor<'a, S, T, F> {
            fn drop(&mut self) {
                if self.poll_fn.is_some() {
                    self.drop_slow()
                }
            }
        }

        impl<'a, S: Source, T, F: FnMut(&mut Context<'_>) -> Poll<T> + 'a + Unpin> WaitFor<'a, S, T, F> {
            #[cold]
            fn drop_slow(&mut self) {
                if self
                    .pollable
                    .with_waker(self.kind, |waker| waker.detach())
                    .is_some()
                {
                    let pending = self.pollable.driver.pending.fetch_sub(1, Ordering::Relaxed);
                    assert_ne!(pending, 0);
                }
            }
        }

        WaitFor {
            poll_fn: Some(poll_fn),
            pollable: self,
            kind,
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

        let result = ready!(do_poll());

        state.tick += 1;
        state.be_fair = state.tick == 0;
        self.state = state.into();

        Poll::Ready(result)
    }
}
