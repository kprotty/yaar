// Copyright 2020 kprotty
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use crate::{
    core::{ParkResult, Parker},
    event::{AutoResetEvent, AutoResetEventTimed, YieldContext},
};
use core::sync::atomic::{AtomicU8, Ordering};

#[cfg(feature = "os")]
use crate::event::OsAutoResetEvent;

/// A [`GenericRawMutex`] backed by [`OsAutoResetEvent`].
#[cfg(feature = "os")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type OsRawMutex = GenericRawMutex<OsAutoResetEvent>;

const UNLOCKED: u8 = 0b00;
const LOCKED: u8 = 0b01;
const PARKED: u8 = 0b10;

const DEFAULT_TOKEN: usize = 0;
const RETRY_TOKEN: usize = 1;
const HANDOFF_TOKEN: usize = 2;

/// A [`lock_api::RawMutex`] implementation based on [`Parker`] and
/// parking_lot's [`RawMutex`].
///
/// [`RawMutex`]: https://docs.rs/parking_lot/0.10.2/src/parking_lot/raw_mutex.rs.html
pub struct GenericRawMutex<E> {
    state: AtomicU8,
    parker: Parker<E>,
}

impl<E> GenericRawMutex<E> {
    /// Create an unlocked RawMutex
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(UNLOCKED),
            parker: Parker::new(),
        }
    }
}

impl<E: AutoResetEvent> GenericRawMutex<E> {
    /// Acquire the mutex using the parking strategy provided.
    ///
    /// `try_park`, along with this function, returns if it was cancelled or
    /// acquired.
    #[inline]
    fn acquire(&self, try_park: impl FnMut(&E) -> bool) -> bool {
        // fast-path: speculatively acquire the mutex
        match self.state.compare_exchange_weak(
            UNLOCKED,
            LOCKED,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            Ok(_) => true,
            Err(_) => self.acquire_slow(try_park),
        }
    }

    #[cold]
    fn acquire_slow(&self, mut try_park: impl FnMut(&E) -> bool) -> bool {
        let mut spin: usize = 0;
        let mut state = self.state.load(Ordering::Relaxed);

        loop {
            // Try to acquire the mutex if its unlocked.
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

            // Spin on the mutex if the Event implementation deems appropriate
            if E::yield_now(YieldContext {
                contended: state & PARKED != 0,
                iteration: spin,
                _sealed: (),
            }) {
                spin = spin.wrapping_add(1);
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            // Mark that theres a parked thread / the mutex is under contention.
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

            // Park on the mutexe's parker until notified via a call to unpark()
            //
            // Safety:
            //
            // The callback functions do not panic or call other Parker functions.
            match unsafe {
                self.parker.park(
                    DEFAULT_TOKEN,
                    |_| self.state.load(Ordering::Relaxed) == LOCKED | PARKED,
                    |event| try_park(event),
                    |was_last_thread| {
                        if was_last_thread {
                            self.state.fetch_and(!PARKED, Ordering::Relaxed);
                        }
                    },
                )
            } {
                // The park function was cancelled (e.g. timed out)
                ParkResult::Cancelled => return false,

                // The thread that unparked us passed the lock directly to us without unlocking.
                ParkResult::Unparked(HANDOFF_TOKEN) => return true,

                // We were unparked normally, try to acquire the mutex again.
                ParkResult::Unparked(RETRY_TOKEN) => {}

                // The mutex was unlocked or woken up when we tried to park.
                // Try to acquire the mutex again.
                ParkResult::Invalid => {}

                _ => unreachable!(),
            }

            // Loop back and try to acquire the mutex again.
            spin = 0;
            state = self.state.load(Ordering::Relaxed);
        }
    }

    #[inline]
    fn release(&self, be_fair: bool) {
        // fast-path: release the mutex assuming no contention
        if self
            .state
            .compare_exchange(LOCKED, UNLOCKED, Ordering::Release, Ordering::Relaxed)
            .is_err()
        {
            self.release_slow(be_fair);
        }
    }

    #[cold]
    fn release_slow(&self, be_fair: bool) {
        // Unpark one thread to be woken up to retry at acquiring the mutex.
        unsafe {
            self.parker.unpark_one(
                |_, token| {
                    // If we're doing a fair unlock, notify the unlocked thread that the mutex will
                    // still be locked.
                    debug_assert_eq!(token, DEFAULT_TOKEN);
                    if be_fair {
                        HANDOFF_TOKEN
                    } else {
                        RETRY_TOKEN
                    }
                },
                |result| {
                    // Keep the mutex locked if doing a fair unlock.
                    // Also unset the PARKED bit if theres no more threads.
                    if result.unparked != 0 && be_fair {
                        if !result.has_more {
                            self.state.store(LOCKED, Ordering::Relaxed);
                        }
                        return;
                    }

                    // Clear the LOCKED bit.
                    // Also clear the PARKED bit if this was the last thread to wake up.
                    if result.has_more {
                        self.state.store(PARKED, Ordering::Release);
                    } else {
                        self.state.store(UNLOCKED, Ordering::Release);
                    }
                },
            )
        }
    }

    #[cold]
    fn bump_slow(&self) {
        self.release_slow(true);
        let acquired = self.acquire(|event| {
            event.wait();
            true
        });
        debug_assert!(acquired);
    }
}

unsafe impl<E: AutoResetEvent> lock_api::RawMutex for GenericRawMutex<E> {
    const INIT: Self = Self::new();

    type GuardMarker = lock_api::GuardSend;

    #[inline]
    fn try_lock(&self) -> bool {
        let mut state = self.state.load(Ordering::Relaxed);
        while state & LOCKED == 0 {
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
        false
    }

    #[inline]
    fn lock(&self) {
        let acquired = self.acquire(|event| {
            event.wait();
            true
        });
        debug_assert!(acquired);
    }

    #[inline]
    fn unlock(&self) {
        self.release(false);
    }
}

unsafe impl<E: AutoResetEvent> lock_api::RawMutexFair for GenericRawMutex<E> {
    #[inline]
    fn unlock_fair(&self) {
        self.release(true)
    }

    #[inline]
    fn bump(&self) {
        if self.state.load(Ordering::Relaxed) & PARKED != 0 {
            self.bump_slow();
        }
    }
}

unsafe impl<E: AutoResetEventTimed> lock_api::RawMutexTimed for GenericRawMutex<E> {
    type Instant = E::Instant;
    type Duration = E::Duration;

    #[inline]
    fn try_lock_until(&self, timeout: Self::Instant) -> bool {
        self.acquire(|event| event.try_wait_until(&timeout))
    }

    #[inline]
    fn try_lock_for(&self, mut timeout: Self::Duration) -> bool {
        self.acquire(|event| event.try_wait_for(&mut timeout))
    }
}
