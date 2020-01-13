
use core::task::Poll;

pub trait ThreadParker: Sized + Sync {
    type Context;

    fn from(context: Context) -> Self;

    fn park(&self) -> Poll<()>;

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
