
#[cfg(all(feature = "os", windows))]
mod windows;
#[cfg(all(feature = "os", windows))]
pub use windows::OsEvent;

#[cfg(all(feature = "os", target_os = "linux"))]
mod linux;
#[cfg(all(feature = "os", target_os = "linux"))]
pub use linux::OsEvent;

#[cfg(all(feature = "os", unix, not(target_os = "linux")))]
mod posix;
#[cfg(all(feature = "os", unix, not(target_os = "linux")))]
pub use posix::OsEvent;

pub unsafe trait Event: Sized + Default + Send + Sync {
    /// Restore an event back to it's unset state.
    /// Since this owns the Event, it can be done without synchronization
    /// and could be potentially cheaper than dropping & creating a new Event.
    fn reset(&mut self);

    /// Wait for the Event to be set by another thread.
    /// If the event isn't set, this should block the current thread until it is.
    /// This will be used assuming an Acquire memory ordering.
    fn wait(&self);

    /// Set the Event, waking up the thread that is waiting for it if any.
    /// This will be used assuming a Release memory ordering.
    fn set(&self);
}
