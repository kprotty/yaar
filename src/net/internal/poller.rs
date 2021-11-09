use super::wakers::{WakerEntry, WakerIndex, WakerKind, WakerStorage};
use crate::runtime::internal::context::Context;
use mio::event::Source;
use std::{
    io,
    sync::atomic::{AtomicUsize, Ordering},
    sync::Arc,
    time::Duration,
};
use try_lock::{Locked, TryLock};

pub struct Poller {
    pending: AtomicUsize,
    waker_storage: WakerStorage,
    registry: mio::Registry,
    selector: TryLock<mio::Poll>,
    signal: mio::Waker,
}

impl Poller {
    pub fn new() -> io::Result<Self> {
        let selector = mio::Poll::new()?;
        let registry = selector.registry().try_clone()?;
        let signal = mio::Waker::new(&registry, mio::Token(usize::MAX))?;

        Ok(Self {
            pending: AtomicUsize::new(0),
            waker_storage: WakerStorage::new(),
            registry,
            selector: TryLock::new(selector),
            signal,
        })
    }

    pub fn with<F>(f: impl FnOnce(&Arc<Self>) -> F) -> F {
        Context::with(|context| f(&context.executor.net_poller))
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

    pub(super) fn io_pending_begin(&self) {
        let pending = self.pending.fetch_add(1, Ordering::Relaxed);
        assert_ne!(pending, usize::MAX);
    }

    pub(super) fn io_pending_complete(&self) {
        let pending = self.pending.fetch_sub(1, Ordering::Relaxed);
        assert_ne!(pending, 0);
    }

    pub fn notify(&self) {
        self.signal.wake().expect("failed to notify the net poller");
    }

    pub fn try_poll(&self) -> Option<PollGuard<'_>> {
        if self.pending.load(Ordering::Acquire) == 0 {
            return None;
        }

        self.selector
            .try_lock()
            .map(|selector| PollGuard { selector })
    }
}

pub struct PollGuard<'a> {
    selector: Locked<'a, mio::Poll>,
}

impl<'a> PollGuard<'a> {
    pub fn poll(mut self, poll_events: &mut PollEvents, timeout: Option<Duration>) {
        loop {
            match self.selector.poll(&mut poll_events.events, timeout) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                _ => break,
            }
        }
    }
}

pub struct PollEvents {
    events: mio::event::Events,
}

impl Default for PollEvents {
    fn default() -> Self {
        Self {
            events: mio::event::Events::with_capacity(256),
        }
    }
}

impl PollEvents {
    pub fn process(&self, poller: &Poller) {
        for event in self.events.iter() {
            let index = match event.token() {
                mio::Token(usize::MAX) => continue,
                mio::Token(token) => WakerIndex::from(token),
            };

            poller.with_wakers(index, |wakers| {
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
