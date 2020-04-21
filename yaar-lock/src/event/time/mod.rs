// Copyright 2020 kprotty
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use core::{
    convert::TryInto,
    ops::{Add, AddAssign, Sub, SubAssign},
    time::Duration,
};

/// Sources and code documentation copied and modified from
/// [`std::time::Instant`].
///
/// [`std::time::Instant`]: https://github.com/rust-lang/rust/blob/d3c79346a3e7ddbb5fb417810f226ac5a9209007/src/libstd/time.rs

#[cfg(all(feature = "os", windows))]
mod windows;
#[cfg(all(feature = "os", windows))]
use windows::*;

#[cfg(all(feature = "os", unix))]
mod posix;
#[cfg(all(feature = "os", unix))]
use posix::*;

/// OS-specific duration types for synchronization primitives
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
pub type OsDuration = Duration;

/// A measurement of a monotonically nondecreasing clock often used for
/// measuring the time of an operation.
///
/// OsInstants represent a moment in time and are useful when measuring the
/// duration or comparing two instants.
///
/// OsInstants are guaranteed to be no less than any previously measured
/// instant. They're not, however, guaranteed to advance in a steady manner
/// (e.g. some seconds may longer or shorter than others).
#[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
#[derive(Copy, Clone, Eq, Ord, PartialEq, PartialOrd, Hash, Debug)]
pub struct OsInstant {
    timestamp: u64,
}

impl AddAssign<Duration> for OsInstant {
    fn add_assign(&mut self, other: Duration) {
        *self = *self + other;
    }
}

impl Add<Duration> for OsInstant {
    type Output = Self;

    fn add(self, other: Duration) -> Self::Output {
        self.checked_add(other)
            .expect("overflow when adding duration to instant")
    }
}

impl SubAssign<Duration> for OsInstant {
    fn sub_assign(&mut self, other: Duration) {
        *self = *self - other;
    }
}

impl Sub<Duration> for OsInstant {
    type Output = Self;

    fn sub(self, other: Duration) -> Self::Output {
        self.checked_sub(other)
            .expect("overflow when subtracting duration from instant")
    }
}

impl Sub<Self> for OsInstant {
    type Output = Duration;

    fn sub(self, other: Self) -> Self::Output {
        self.duration_since(other)
    }
}

impl OsInstant {
    /// Get the current timestamp from the OS with a nondecreasing guarantee.
    fn timestamp() -> u64 {
        unsafe {
            // Return the timestamp immediately if the OS is assumed
            // to guarantee a monotonic increasing timestamp.
            let mut now = Timer::timestamp();
            if Timer::IS_ACTUALLY_MONOTONIC {
                return now;
            }

            // If not, acquire the lock on the current timestamp and update the global
            // value. We do this to ensure that the timestamp reported by the OS
            // doesn't go backwards.

            // On 64-bit & higher platforms its more efficient to use a cas loop
            #[cfg(target_pointer_width = "64")]
            {
                use core::sync::atomic::{AtomicU64, Ordering};
                static CURRENT: AtomicU64 = AtomicU64::new(0);

                let mut current = CURRENT.load(Ordering::Relaxed);
                loop {
                    if current >= now {
                        now = current;
                        break;
                    } else {
                        match CURRENT.compare_exchange_weak(
                            current,
                            now,
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => break,
                            Err(e) => current = e,
                        }
                    }
                }
            }

            // On platforms that dont support 64bit load & cas, use a Lock
            #[cfg(not(target_pointer_width = "64"))]
            {
                use crate::{core::Lock, event::OsAutoResetEvent};
                use lock_api::RawMutex;

                static mut CURRENT: u64 = 0;
                static LOCK: Lock<OsAutoResetEvent> = Lock::new();

                LOCK.lock();
                if CURRENT >= now {
                    now = CURRENT;
                } else {
                    CURRENT = now;
                }
                LOCK.unlock();
            }

            now
        }
    }

    /// Returns an instant corresponding to the current moment in time.
    pub fn now() -> Self {
        Self {
            timestamp: Self::timestamp(),
        }
    }

    /// Returns the amount of time elapsed from another instant to this one.
    ///
    /// # Panics
    ///
    /// This function will panic if `earlier` is later than `self`.
    pub fn duration_since(&self, earlier: Self) -> Duration {
        self.checked_duration_since(earlier)
            .expect("Supplied instant is later than self")
    }

    /// Returns the amount of time elapsed from another instant to this one,
    /// or None if that instant is later than this one.
    pub fn checked_duration_since(&self, earlier: Self) -> Option<Duration> {
        self.timestamp
            .checked_sub(earlier.timestamp)
            .map(Duration::from_nanos)
    }

    /// Returns the amount of time elapsed from another instant to this one,
    /// or zero duration if that instant is earlier than this one.
    pub fn saturating_duration_since(&self, earlier: Self) -> Duration {
        self.checked_duration_since(earlier)
            .unwrap_or(Duration::new(0, 0))
    }

    /// Returns the amount of time elapsed since this instant was created.
    ///
    /// # Panics
    ///
    /// This function may panic if the current time is earlier than this
    /// instant, which is something that can happen if an `Instant` is
    /// produced synthetically.
    pub fn elapsed(&self) -> Duration {
        Self::now().duration_since(*self)
    }

    /// Returns `Some(t)` where `t` is the time `self + duration` if `t` can be
    /// represented as `Instant` (which means it's inside the bounds of the
    /// underlying data structure), `None` otherwise.
    pub fn checked_add(&self, duration: Duration) -> Option<Self> {
        duration
            .as_nanos()
            .try_into()
            .ok()
            .and_then(|dur: u64| self.timestamp.checked_add(dur))
            .map(|timestamp| Self { timestamp })
    }

    /// Returns `Some(t)` where `t` is the time `self - duration` if `t` can be
    /// represented as `Instant` (which means it's inside the bounds of the
    /// underlying data structure), `None` otherwise.
    pub fn checked_sub(&self, duration: Duration) -> Option<Self> {
        duration
            .as_nanos()
            .try_into()
            .ok()
            .and_then(|dur: u64| self.timestamp.checked_sub(dur))
            .map(|timestamp| Self { timestamp })
    }
}
