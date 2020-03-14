

pub unsafe trait Signal: Sync {
    fn wait(&self);

    fn notify(&self);
}

#[cfg(all(feature = "os", target_os = "windows"))]
mod windows;
#[cfg(all(feature = "os", target_os = "windows"))]
pub use windows::OsSignal;

#[cfg(all(feature = "os", target_os = "linux"))]
mod linux;
#[cfg(all(feature = "os", target_os = "linux"))]
pub use linux::OsSignal;

#[cfg(all(feature = "os", unix, not(target_os = "linux")))]
mod posix;
#[cfg(all(feature = "os", unix, not(target_os = "linux")))]
pub use posix::OsSignal;