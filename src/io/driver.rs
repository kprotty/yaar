use super::wakers::{WakerIndex, WakerKind, Wakers};
use crate::sync::waker::AtomicWaker;
use mio::event::Source;
use std::{
    future::Future,
    io,
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
    pub fn new(mut source: S, driver: &Arc<Driver>) -> io::Result<Self> {
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

    pub fn poll_io<T>(
        &self,
        kind: WakerKind,
        waker_ref: &Waker,
        mut do_io: impl FnMut() -> io::Result<T>,
    ) -> Poll<io::Result<T>> {
        loop {
            let mut waker_token = ready!(self.poll_ready(kind, waker_ref));
            loop {
                match do_io() {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => match kind {
                        WakerKind::Write => continue,
                        WakerKind::Read => {
                            waker_ref.wake_by_ref();
                            return Poll::Pending;
                        }
                    },
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        match self.with_waker(kind, |waker| waker.reset(waker_token)) {
                            Ok(_) => break,
                            Err(token) => waker_token = token,
                        }
                    }
                    result => return Poll::Ready(result),
                }
            }
        }
    }

    pub fn wait_for<'a>(&'a self, kind: WakerKind) -> impl Future<Output = ()> + 'a {
        struct WaitFor<'a, S: Source> {
            pollable: Option<&'a Pollable<S>>,
            kind: WakerKind,
        }

        impl<'a, S: Source> Future for WaitFor<'a, S> {
            type Output = ();

            fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
                let pollable = self
                    .pollable
                    .take()
                    .expect("wait_for() polled after completion");

                if let Poll::Ready(_) = pollable.poll_ready(self.kind, ctx.waker()) {
                    return Poll::Ready(());
                }

                self.pollable = Some(pollable);
                Poll::Pending
            }
        }

        impl<'a, S: Source> Drop for WaitFor<'a, S> {
            fn drop(&mut self) {
                if let Some(pollable) = self.pollable.take() {
                    self.drop_slow(pollable)
                }
            }
        }

        impl<'a, S: Source> WaitFor<'a, S> {
            #[cold]
            fn drop_slow(&mut self, pollable: &'a Pollable<S>) {
                if pollable
                    .with_waker(self.kind, |waker| waker.detach())
                    .is_some()
                {
                    let pending = pollable.driver.pending.fetch_sub(1, Ordering::Relaxed);
                    assert_ne!(pending, 0);
                }
            }
        }

        WaitFor {
            pollable: Some(self),
            kind,
        }
    }
}
