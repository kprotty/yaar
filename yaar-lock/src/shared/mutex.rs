use crate::parker::{
    Parker,
    ParkResult,
    AutoResetEvent,
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
    parker: Parker<Event>,
}

impl<Event> RawMutex<Event> {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            parker: Parker::new(),
        }
    }
}

impl<Event: AutoResetEvent> RawMutex<Event> {
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

    pub async unsafe fn lock_slow(
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

            let validate = || {
                if self.state.load(Ordering::Relaxed) == (LOCKED | PARKED) {
                    Ok(DEFAULT_TOKEN)
                } else {
                    Err(())
                }
            };
            let timed_out = |_token, was_last| {
                if was_last {
                    self.state.fetch_and(!PARKED, Ordering::Relaxed);
                }
            };

            match self.parker.park(
                |event| park(event),
                validate,
                timed_out,
            ).await {
                ParkResult::Unparked(HANDOFF_TOKEN) => return true,
                ParkResult::Unparked(_) => {},
                ParkResult::Cancelled(_) => return false, 
                ParkResult::Unprepared => {},
            }

            spin = 0;
            state = self.state.load(Ordering::Relaxed);
        }
    }

    pub unsafe fn unlock(
        &self,
        be_fair: bool,
        unpark: impl Fn(&Event),
    ) {
        self.parker.unpark_one(unpark, |ctx, token| {
            if ctx.unparked > 0 && be_fair {
                *token = HANDOFF_TOKEN;
                if !ctx.has_more {
                    self.state.store(LOCKED, Ordering::Relaxed);
                }
            } else {
                let new_state = if ctx.has_more { PARKED } else { 0 };
                self.state.store(new_state, Ordering::Release);
            }
        })
    }
} 