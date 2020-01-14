/// Abstraction used for blocking threads until notified.
///
/// A thread parker starts in a state of un-signaled.
/// One or more threads can then `park()` on this instance,
/// blocking the thread until another calls `unpark()`. This
/// function transitions the instance into a signaled state
/// and wakes up all threads blocked `park()`ing. The `reset()`
/// function can be used to restore the instance back into
/// the un-signaled state. Failing to do so while in a signaled
/// state may cause subsequent `park()` calls to return immediately.
pub trait ThreadParker: Default + Sync {
    /// Block the current thread until the parker is transitioned
    /// into a signaled state through a call to `unpark()` either
    /// previously before being reset or in parallel.
    fn park(&self);

    /// Transition the state of the parker to signaled if not already,
    /// waking up all threads blocked via `park()`. Any subsequent calls
    /// to `park()` may then return immediately.
    fn unpark(&self);

    /// Reset the state back to un-signaled, allowing threads to
    /// block on `park()` again and allowing `unpark()` to wake them.
    fn reset(&self);
}

#[cfg(all(feature = "os", windows))]
mod windows;
#[cfg(all(feature = "os", windows))]
use windows::Parker as SystemThreadParker;

#[cfg(all(feature = "os", target_os = "linux"))]
mod linux;
#[cfg(all(feature = "os", target_os = "linux"))]
use linux::Parker as SystemThreadParker;

#[cfg(all(feature = "os", unix, not(target_os = "linux")))]
mod posix;
#[cfg(all(feature = "os", unix, not(target_os = "linux")))]
use posix::Parker as SystemThreadParker;

#[cfg(feature = "os")]
pub use self::if_os::*;
#[cfg(feature = "os")]
mod if_os {
    use super::*;

    /// The defualt [`ThreadParker`] implementation for the OS.
    #[derive(Default)]
    pub struct OsThreadParker(SystemThreadParker);

    unsafe impl Sync for OsThreadParker {}

    impl ThreadParker for OsThreadParker {
        fn park(&self) {
            self.0.park()
        }

        fn unpark(&self) {
            self.0.unpark()
        }

        fn reset(&self) {
            self.0.reset()
        }
    }
}
