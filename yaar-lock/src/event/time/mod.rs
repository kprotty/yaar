use core::{
    mem::size_of,
    ops::{Add, AddAssign, Sub, SubAssign},
    time::Duration,
    sync::atomic::{spin_loop_hint, Ordering, AtomicBool},
};

#[cfg(all(feature = "os", windows))]
mod windows;
#[cfg(all(feature = "os", windows))]
use windows::*;

#[cfg(all(feature = "os", unix))]
mod posix;
#[cfg(all(feature = "os", unix))]
use posix::*;

pub type OsDuration = Duration;

#[derive(Copy, Clone, Eq, Ord, PartialEq, PartialOrd, Hash, Debug)]
pub struct OsInstant {
    timestamp: Duration,
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
    unsafe fn timestamp() -> OsDuration {
        let mut now = Timer::timestamp();
        if Timer::IS_ACTUALLY_MONOTONIC {
            return now;
        }

        static LOCKED: AtomicBool = AtomicBool::new(false);
        static mut CURRENT: OsDuration = OsDuration::from_secs(0);

        let mut spin: usize = 0;
        loop {
            if !LOCKED.load(Ordering::Relaxed) {
                if let Ok(_) = LOCKED.compare_exchange_weak(
                    false,
                    true,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    break;
                }
            }
            spin = spin.wrapping_add(size_of::<usize>());
            (0..spin.min(1024)).for_each(|_| spin_loop_hint());
        }
        
        if CURRENT >= now {
            now = CURRENT;
        } else {
            CURRENT = now;
        }

        LOCKED.store(false, Ordering::Release);
        now
    }

    pub fn now() -> Self {
        Self {
            timestamp: unsafe { Self::timestamp() },
        }
    }

    pub fn duration_since(&self, earlier: Self) -> Duration {
        self.checked_duration_since(earlier)
            .expect("Supplied instant is later than self")
    }

    pub fn checked_duration_since(&self, earlier: Self) -> Option<Duration> {
        self.timestamp.checked_sub(earlier.timestamp)
    }

    pub fn saturating_duration_since(&self, earlier: Self) -> Duration {
        self.checked_duration_since(earlier)
            .unwrap_or(Duration::new(0, 0))
    }

    pub fn elapsed(&self) -> Duration {
        Self::now().duration_since(*self)
    }

    pub fn checked_add(&self, duration: Duration) -> Option<Self> {
        self.timestamp
            .checked_add(duration)
            .map(|timestamp| Self { timestamp })
    }

    pub fn checked_sub(&self, duration: Duration) -> Option<Self> {
        self.timestamp
            .checked_sub(duration)
            .map(|timestamp| Self { timestamp })
    }
}
