use super::{WakerIndex, WakerKind, Wakers};
use std::{
    io, mem,
    sync::atomic::{AtomicUsize, Ordering},
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
        let registry = selector
            .registry()
            .try_clone()?;

        let signal = mio::Waker::new(&registry, mio::Token(usize::MAX))?;
        Ok(Self {
            pending: AtomicUsize::new(0),
            selector: TryLock::new(selector),
            registry,
            signal,
            wakers: Wakers::default(),
        })
    }

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

        loop {
            match selector.poll(&mut self.events, timeout) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                _ => break mem::drop(selector),
            }
        }

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


