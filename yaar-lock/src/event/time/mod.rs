use core::{
    time::Duration,
    ops::{Add, Sub, AddAssign, SubAssign},
};

#[cfg_attr(windows, path = "./windows.rs")]
mod os;

mod monotonic;

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
    pub fn now() -> Self {
        let timestamp = unsafe { monotonic::timestamp::<os::Timer>() };
        Self { timestamp }
    }

    pub fn duration_since(&self, earlier: Self) -> Duration {
        self.checked_duration_since(earlier)
            .expect("Supplied instant is later than self")
    }

    pub fn checked_duration_since(&self, earlier: Self) -> Option<Duration> {
        self.timestamp
            .checked_sub(earlier.timestamp)
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