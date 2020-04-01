pub unsafe trait AutoResetEvent: Sync + Default {
    fn set(&self);

    fn wait(&self);

    /// Hint to the platform that a thread is spinning, similar to
    /// [`spin_loop_hint`].
    ///
    /// Returns whether the thread should continue spinning or not.
    fn yield_now(iteration: usize) -> bool;
}

pub unsafe trait AutoResetEventTimed: AutoResetEvent {
    type Duration;
    type Instant;

    fn try_wait_for(&self, timeout: Self::Duration) -> bool;

    fn try_wait_until(&self, timeout: Self::Instant) -> bool;
}

#[cfg(feature = "os")]
pub use if_os::*;

#[cfg(feature = "os")]
mod if_os {
    use super::*;
    use core::{
        time::Duration,
        ops::{Add, Sub, AddAssign, SubAssign},
        sync::atomic::{Ordering, AtomicUsize},
    };

    #[cfg_attr(windows, path = "../windows.rs")]
    mod os;

    const EMPTY: usize = 0;
    const WAITING: usize = 1;
    const NOTIFIED: usize = 2;

    pub struct OsAutoResetEvent {
        state: AtomicUsize,
    }

    unsafe impl Sync for OsAutoResetEvent {}
    unsafe impl Send for OsAutoResetEvent {}

    impl Default for OsAutoResetEvent {
        fn default() -> Self {
            Self::new()
        }
    }

    unsafe impl AutoResetEventTimed for OsAutoResetEvent {
        type Instant = OsInstant;
        type Duration = Duration;

        fn try_wait_for(&self, timeout: Self::Duration) -> bool {
            self.try_wait(Some(timeout))
        }

        fn try_wait_until(&self, timeout: Self::Instant) -> bool {
            let timeout = OsInstant::now().saturating_duration_since(timeout);
            timeout.as_nanos() != 0 && self.try_wait(Some(timeout))
        }
    }

    unsafe impl AutoResetEvent for OsAutoResetEvent {
        fn set(&self) {
            self.notify();
        }

        fn wait(&self) {
            let notified = self.try_wait(None);
            debug_assert!(notified);
        }

        fn yield_now(iteration: usize) -> bool {
            os::yield_now(iteration)
        }
    }

    impl OsAutoResetEvent {
        pub const fn new() -> Self {
            Self {
                state: AtomicUsize::new(EMPTY)
            }
        }

        fn notify(&self) {
            #[cold]
            fn wake(ptr: &AtomicUsize) {
                unsafe { os::futex_wake(ptr) }
            }

            let mut state = EMPTY;
            while state != NOTIFIED {
                match self.state.compare_exchange_weak(
                    state,
                    if state == EMPTY { NOTIFIED } else { EMPTY },
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Err(e) => state = e,
                    Ok(WAITING) => return wake(&self.state),
                    Ok(_) => return,
                }
            }
        }

        fn try_wait(&self, timeout: Option<Duration>) -> bool {
            #[cold]
            fn wait(ptr: &AtomicUsize, timeout: Option<Duration>) -> bool {
                unsafe { os::futex_wait(ptr, WAITING, EMPTY, timeout) }
            }

            let mut state = NOTIFIED;
            loop {
                match self.state.compare_exchange_weak(
                    state,
                    if state == NOTIFIED { EMPTY } else { WAITING },
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Err(e) => state = e,
                    Ok(NOTIFIED) => return true,
                    Ok(_) => return wait(&self.state, timeout), 
                }
            }
        }
    }

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
            let timestamp = unsafe { os::timestamp() };
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
}