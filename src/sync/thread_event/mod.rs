/// An abstraction for blocking and notifying of the system's execution threads.
///
/// A ThreadEvent starts out in an unsignaled state. A thread may wait for the
/// event to become signaled using the `wait` function. Another thread can then
/// call `notify` which transitions the event into a signaled state which
/// unblocks any threads currently waiting on the event. Future attempts to wait
/// on the event after being set should observe this signal state and return
/// immediately until the event is `reset` back to and unsignaled state.
pub trait ThreadEvent: Default + Sync {
    /// Restore the event to an unsignaled state.
    fn reset(&self);

    /// Wait for the event to be signaled by blocking the current thread.
    fn wait(&self);

    /// Transition to a signaled state, unblocking any threads waiting on the
    /// event.
    fn notify(&self);
}

#[cfg(all(feature = "os", windows))]
mod windows;
#[cfg(all(feature = "os", windows))]
use windows::Event as SystemEvent;

#[cfg(all(feature = "os", target_os = "linux"))]
mod linux;
#[cfg(all(feature = "os", target_os = "linux"))]
use linux::Event as SystemEvent;

#[cfg(all(feature = "os", unix, not(target_os = "linux")))]
mod posix;
#[cfg(all(feature = "os", unix, not(target_os = "linux")))]
use posix::Event as SystemEvent;

#[cfg(feature = "os")]
pub use self::if_os::*;
#[cfg(feature = "os")]
mod if_os {
    /// The default ThreadEvent implementation using the OS primitives.
    #[cfg_attr(feature = "nightly", doc(cfg(feature = "os")))]
    #[derive(Default)]
    pub struct OsThreadEvent(super::SystemEvent);

    unsafe impl Sync for OsThreadEvent {}

    impl super::ThreadEvent for OsThreadEvent {
        #[inline]
        fn reset(&self) {
            self.0.reset()
        }

        #[inline]
        fn notify(&self) {
            self.0.notify()
        }

        #[inline]
        fn wait(&self) {
            self.0.wait()
        }
    }
}
