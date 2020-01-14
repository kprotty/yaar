
pub trait ThreadParker: Default + Sync {
    fn park(&self);

    fn unpark(&self);

    fn reset(&self);
}

#[cfg(all(feature = "os", windows))]
mod windows;
#[cfg(all(feature = "os", windows))]
pub use windows::Parker as OsThreadParker;

#[cfg(all(feature = "os", target_os = "linux"))]
mod linux;
#[cfg(all(feature = "os", target_os = "linux"))]
pub use linux::Parker as OsThreadParker;

#[cfg(all(feature = "os", unix, not(target_os = "linux")))]
mod posix;
#[cfg(all(feature = "os", unix, not(target_os = "linux")))]
pub use posix::Parker as OsThreadParker;
