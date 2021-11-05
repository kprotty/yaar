use super::waker::{WakerEntry, WakerIndex, WakerKind, WakerStorage};
use crate::runtime::scheduler::context::Context;
use mio::event::Source;
use std::{io, sync::Arc, time::Duration};
use try_lock::{Locked, TryLock};

pub struct Driver {
    waker_storage: WakerStorage,
    registry: mio::Registry,
    selector: TryLock<mio::Poll>,
    signal: mio::Waker,
}

impl Driver {
    pub fn new() -> io::Result<Self> {
        let selector = mio::Poll::new()?;
        let registry = selector.registry().try_clone()?;
        let signal = mio::Waker::new(&registry, mio::Token(usize::MAX))?;

        Ok(Self {
            waker_storage: WakerStorage::new(),
            registry,
            selector: TryLock::new(selector),
            signal,
        })
    }

    pub fn notify(&self) {
        self.signal.wake().expect("failed to notify the I/O driver");
    }

    pub fn try_poll(&self) -> Option<PollGuard<'_>> {
        self.selector
            .try_lock()
            .map(|selector| PollGuard { selector })
    }

    pub(super) fn with<F>(f: impl FnOnce(&Arc<Self>) -> F) -> F {
        let context_ref = Context::current();
        f(&context_ref.as_ref().executor.io_driver)
    }

    pub(super) fn register<S: Source>(&self, source: &mut S) -> io::Result<WakerIndex> {
        let index = self
            .waker_storage
            .alloc()
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;

        self.registry
            .register(
                source,
                mio::Token(index.into()),
                mio::Interest::READABLE | mio::Interest::WRITABLE,
            )
            .map(|_| index)
            .map_err(|error| {
                self.waker_storage.free(index);
                error
            })
    }

    pub(super) fn deregister<S: Source>(&self, source: &mut S, index: WakerIndex) {
        // Ignore deregister I/O errors (tokio does this too)
        let _ = self.registry.deregister(source);
        self.waker_storage.free(index);
    }

    pub(super) fn with_wakers<F>(&self, index: WakerIndex, f: impl FnOnce(&WakerEntry) -> F) -> F {
        self.waker_storage.with(index, f)
    }
}

pub struct PollGuard<'a> {
    selector: Locked<'a, mio::Poll>,
}

impl<'a> PollGuard<'a> {
    pub fn poll(&mut self, events: &mut PollEvents, timeout: Option<Duration>) {
        let _ = self.selector.poll(&mut events.events, timeout);
    }
}

pub struct PollEvents {
    events: mio::event::Events,
}

impl PollEvents {
    pub fn new() -> Self {
        Self {
            events: mio::event::Events::with_capacity(256),
        }
    }

    pub fn process(&self, driver: &Driver) {
        for event in self.events.iter() {
            let index = match event.token() {
                mio::Token(usize::MAX) => continue,
                mio::Token(token) => WakerIndex::from(token),
            };

            driver.with_wakers(index, |wakers| {
                if event.is_readable() {
                    wakers[WakerKind::Read as usize].wake();
                }

                if event.is_writable() {
                    wakers[WakerKind::Write as usize].wake();
                }
            })
        }
    }
}
