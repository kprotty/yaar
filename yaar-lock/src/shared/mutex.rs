use crate::AutoResetEvent;
use super::parker::{
    Parker,
    ParkResult,
};
use core::{
    task::Poll,
    sync::atomic::{Ordering, AtomicUsize},
};

const LOCKED: usize = 1 << 0;
const PARKED: usize = 1 << 1;

const DEFAULT_TOKEN: usize = 1 << 0;
const HANDOFF_TOKEN: usize = 1 << 1;

pub struct RawMutex<Event> {
    state: AtomicUsize,
    parker: Parker<Event, usize>,
}

impl<Event: AutoResetEvent + Default> RawMutex<Event> {
    pub fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            parker: Parker::new(),
        }
    }

    pub fn try_lock(&self) -> bool {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state & LOCKED != 0 {
                return false;
            }
            match self.state.compare_exchange_weak(
                state,
                state | LOCKED,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(e) => state = e,
            }
        }
    }

    pub fn lock_fast(&self) -> bool {
        self.state
            .compare_exchange_weak(0, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    pub unsafe async fn lock_slow(
        &self,
        park: impl Fn(&Event) -> Poll<bool>,
    ) -> bool {
        let mut spin: usize = 0;
        let mut state = self.state.load(Ordering::Relaxed);

        loop {
            if state & LOCKED == 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state | LOCKED,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return true,
                    Err(e) => state = e,
                }
                continue;
            }

            if (state & PARKED == 0) && !Event::yield_now(spin) {
                spin = spin.wrapping_add(1);
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            if state & PARKED == 0 {
                if let Err(e) = self.state.compare_exchange_weak(
                    state,
                    state | PARKED,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    state = e;
                    continue;
                }
            }

            let validate = |_, _| self.state.load(Ordering::Relaxed) == LOCKED | PARKED;
            let enqueued = |waiters, _| waiters += 1;
            let cancelled = |waiters, _| {
                waiters -= 1;
                if *waiters == 0 {
                    self.state.fetch_and(!PARKED, Ordering::Relaxed);
                }
            };

            match self.parker.park(
                || Event::default(),
                |event| park(event),
                || {
                    if self.state.load(Ordering::Relaxed) == (LOCKED | PARKED) {
                        Ok(DEFAULT_TOKEN)
                    } else {
                        Err(())
                    }
                },
                |_| {},
            ).await {
                ParkResult::Unparked(HANDOFF_TOKEN) => return true,
                ParkResult::Unparked(_) => {},
                ParkResult::Invalid => {},
                ParkResult::Cancelled => return false, 
            }

            spin = 0;
            state = self.state.load(Ordering::Relaxed);
        }
    }

    pub unsafe fn unlock(&self, is_fair: bool) {
        self.parker.unpark()
    }
} 