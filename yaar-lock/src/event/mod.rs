/// Hints towards the synchronization situation provided to an implementation of
/// [`AutoResetEvent`].
///
/// An implementation of [`AutoResetEvent::yield_now`] can use this information
/// to influence scheduling.
pub struct YieldContext {
    /// True if a synchronized resource is trying to be accesed by multiple
    /// threads.
    pub contended: bool,
    /// The amount of successful `yield_now`s called in succession while trying
    /// to access a synchronized resource.
    pub iteration: usize,
    /// Dummy field to allow future modifications
    pub(crate) _sealed: (),
}

/// The primary abstraction upon which all thread synchronization primitives are
/// built on.
///
/// An AutoResetEvent serves as the interface for blocking and unblocking
/// threads along with providing hints to the underlying thread scheduler when
/// attempting to yield.
///
/// A thread which calls `wait` will wait for the event to be set, consume its
/// state, and unset the event. Another thread which calls `set` will set the
/// event if not already and wake up a waiting thread to consume its state.
/// The concept is similar to a semaphore with 1 permit or a synchronous version
/// of tokio's [`Notify`].
///
/// There should only be one thread calling set and another thread calling wait
/// meaning implementations are free to optimize for this Single-Producer,
/// Single-Consumer like access. In accordance to this, the `set` and `wait`
/// functions must also ensure [`Release`] and [`Acquire`] memory orderings
/// accordingly.
///
/// # Safety
///
/// Implementations of this trait must ensure that a thread waiting for the
/// event to be set does not spuriously resume and is only resumed by a
/// corresponding call to `set` from another thread in order to avoid data
/// races. The `wait` function must also unset the event according to the
/// "Reset" aspect and the `set` function must wake up a waiting thread if any
/// in order to avoid deadlocks.
///
/// [`Notify`]: https://docs.rs/tokio/0.2.18/tokio/sync/struct.Notify.html
/// [`Acquire`]: https://doc.rust-lang.org/core/sync/atomic/enum.Ordering.html#variant.Acquire
/// [`Release`]: https://doc.rust-lang.org/core/sync/atomic/enum.Ordering.html#variant.Release
pub unsafe trait AutoResetEvent: Sync + Sized + Default {
    /// Set the event if not set already and wake up any waiting threads wishing
    /// to unset/consume the event.
    ///
    /// # Important
    ///
    /// This function MUST ensure a [`Release`] memory ordering for correctness
    /// in use.
    ///
    /// [`Release`]: https://doc.rust-lang.org/core/sync/atomic/enum.Ordering.html#variant.Release
    fn set(&self);

    /// Blocks the current thread until the event becomes set, then
    /// unset/consume the event.
    ///
    /// # Important
    ///
    /// This function MUST ensure an [`Acquire`] memory ordering for correctness
    /// in use.
    ///
    /// [`Acquire`]: https://doc.rust-lang.org/core/sync/atomic/enum.Ordering.html#variant.Acquire
    fn wait(&self);

    /// Hint to the thread scheduler that the current thread wishes to yield
    /// based on the given `context`. Returns whether or not this thread is
    /// allowed to yield.
    ///
    /// This is used to influence the implementations of synchronization
    /// algorithms as well as provide hooks into their interactions.
    ///
    /// # Example
    ///
    /// To force a synchronization algorithm to spin instead of fallback to
    /// `wait`/`set`, `yield_now` could always return true.
    fn yield_now(context: YieldContext) -> bool;
}

/// Addition methods for AutoResetEvent's which support waiting with timeouts.
pub unsafe trait AutoResetEventTimed: AutoResetEvent {
    /// Represents a relative moment in time to wait for until timing out.
    type Instant;
    /// Represents a fixed amount of time to wait for until timing out.
    type Duration;

    /// Wait until a relative instant in time before timing out.
    /// Returns true if the event was unset or false if it timed out waiting for
    /// the event to be set.
    ///
    /// This function takes a reference in order to avoid requiring impl Copy.
    fn try_wait_until(&self, timeout: &Self::Instant) -> bool;

    /// Wait for a fixed amount of time before timing out.
    /// Returns true if the event was unset or false if it timed out waiting for
    /// the event to be set.
    ///
    /// If the return value is true, the function updates the timeout with the
    /// subtracted amount of time it spent waiting for the event.
    fn try_wait_for(&self, timeout: &mut Self::Duration) -> bool;
}

#[cfg(all(feature = "os", windows))]
mod windows;
#[cfg(all(feature = "os", windows))]
use windows::*;

#[cfg(all(feature = "os", any(target_os = "linux", target_os = "android")))]
mod linux;
#[cfg(all(feature = "os", any(target_os = "linux", target_os = "android")))]
use linux::*;

#[cfg(all(
    feature = "os",
    unix,
    not(any(target_os = "linux", target_os = "android"))
))]
mod posix;
#[cfg(all(
    feature = "os",
    unix,
    not(any(target_os = "linux", target_os = "android"))
))]
use posix::*;

#[cfg(feature = "os")]
mod time;
#[cfg(feature = "os")]
pub use time::*;

#[cfg(feature = "os")]
pub use if_os::*;
#[cfg(feature = "os")]
mod if_os {
    use super::*;
    use core::sync::atomic::spin_loop_hint;

    /// A default implementation of [`AutoResetEvent`] and
    /// [`AutoResetEventTimed`] which uses OS primitives for supported
    /// platforms.
    #[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
    #[derive(Debug)]
    pub struct OsAutoResetEvent {
        signal: Signal,
    }

    unsafe impl Sync for OsAutoResetEvent {}
    unsafe impl Send for OsAutoResetEvent {}

    impl Default for OsAutoResetEvent {
        fn default() -> Self {
            Self::new()
        }
    }

    impl OsAutoResetEvent {
        pub const fn new() -> Self {
            Self {
                signal: Signal::new(),
            }
        }
    }

    unsafe impl AutoResetEvent for OsAutoResetEvent {
        fn set(&self) {
            self.signal.notify();
        }

        fn wait(&self) {
            let notified = self.signal.try_wait(None);
            debug_assert!(notified);
        }

        fn yield_now(context: YieldContext) -> bool {
            // Backoff spinning when uncontended appears to be best in benchmarks
            // for reducing the amount of contention on synchronization primitives.

            const SPINS: usize = 1024;
            const BITS: usize = (SPINS - 1).count_ones() as usize;

            if !context.contended {
                if context.iteration < BITS {
                    (0..(1 << context.iteration)).for_each(|_| spin_loop_hint());
                    return true;
                } else if context.iteration < BITS + 10 {
                    let spin = context.iteration - (BITS - 1);
                    (0..(spin * SPINS)).for_each(|_| spin_loop_hint());
                    return true;
                }
            }

            false
        }
    }

    unsafe impl AutoResetEventTimed for OsAutoResetEvent {
        type Instant = OsInstant;
        type Duration = OsDuration;

        fn try_wait_until(&self, timeout: &Self::Instant) -> bool {
            self.signal.try_wait(Some(timeout))
        }

        fn try_wait_for(&self, timeout: &mut Self::Duration) -> bool {
            let times_out = OsInstant::now() + *timeout;
            let result = self.try_wait_until(&times_out);
            *timeout = times_out.saturating_duration_since(OsInstant::now());
            result
        }
    }
}
