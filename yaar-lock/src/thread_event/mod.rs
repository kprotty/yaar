/// Abstraction which provides a method of blocking and unblocking threads
/// through the interface of a manual reset event.
pub trait ThreadEvent: Sync + Default {
    /// Approximately check if the event is set or not.
    fn is_set(&self) -> bool;

    /// Reset the event back to an un-signalled state.
    fn reset(&self);

    /// Block for the event to become signaled.
    /// If the event is already signaled, this returns without blocking.
    fn wait(&self);

    /// Transition the event to a signaled state,
    /// unblocking any threads that were waiting on it.
    fn notify(&self);
}

#[cfg(all(feature = "os", windows))]
mod windows;
#[cfg(all(feature = "os", windows))]
use windows::Event as SystemThreadEvent;

#[cfg(all(feature = "os", target_os = "linux"))]
mod linux;
#[cfg(all(feature = "os", target_os = "linux"))]
use linux::Event as SystemThreadEvent;

#[cfg(all(feature = "os", not(target_os = "linux")))]
mod posix;
#[cfg(all(feature = "os", not(target_os = "linux")))]
use posix::Event as SystemThreadEvent;

#[cfg(feature = "os")]
pub use self::if_os::*;
#[cfg(feature = "os")]
mod if_os {
    use super::{SystemThreadEvent, ThreadEvent};
    use core::fmt;

    /// The default ThreadEvent which uses the platform's blocking primitives.
    #[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
    #[derive(Default)]
    pub struct OsThreadEvent(SystemThreadEvent);

    unsafe impl Sync for OsThreadEvent {}
    unsafe impl Send for OsThreadEvent {}

    impl fmt::Debug for OsThreadEvent {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("OsThreadEvent")
                .field("is_set", &self.0.is_set())
                .finish()
        }
    }

    impl ThreadEvent for OsThreadEvent {
        fn is_set(&self) -> bool {
            self.0.is_set()
        }

        fn reset(&self) {
            self.0.reset()
        }

        fn notify(&self) {
            self.0.notify()
        }

        fn wait(&self) {
            self.0.wait()
        }
    }
}
