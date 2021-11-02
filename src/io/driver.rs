use super::wakers::{WakerIndex, WakerKind, Wakers};
use std::{
    io, mem,
    sync::atomic::{AtomicUsize, Ordering},
    task::Waker,
    time::Duration,
};
use try_lock::TryLock;

pub struct Driver {
    pub(super) pending: AtomicUsize,
    pub(super) registry: mio::Registry,
    pub(super) wakers: Wakers,
    selector: TryLock<mio::Poll>,
    signal: mio::Waker,
}

impl Driver {
    pub fn new() -> io::Result<Self> {
        let selector = mio::Poll::new()?;
        let registry = selector.registry().try_clone()?;
        let signal = mio::Waker::new(&registry, mio::Token(usize::MAX))?;

        Ok(Self {
            pending: AtomicUsize::new(0),
            registry,
            selector: TryLock::new(selector),
            signal,
            wakers: Wakers::default(),
        })
    }

    pub fn notify(&self) {
        self.signal.wake().expect("failed to wake I/O driver");
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

        if driver.pending.load(Ordering::Relaxed) == 0 {
            return;
        }

        let _ = selector.poll(&mut self.events, timeout);
        mem::drop(selector);

        let mut resumed = 0;
        for event in self.events.iter() {
            let index = match event.token() {
                mio::Token(usize::MAX) => continue,
                mio::Token(index) => WakerIndex::from(index),
            };

            driver.wakers.with(index, |wakers| {
                if event.is_readable() {
                    resumed += wakers[WakerKind::Read as usize]
                        .wake()
                        .map(Waker::wake)
                        .is_some() as usize;
                }

                if event.is_writable() {
                    resumed += wakers[WakerKind::Write as usize]
                        .wake()
                        .map(Waker::wake)
                        .is_some() as usize;
                }
            });
        }

        if resumed > 0 {
            let pending = driver.pending.fetch_sub(resumed, Ordering::Relaxed);
            assert!(pending >= resumed);
        }
    }
}
