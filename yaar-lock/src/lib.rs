//! Fast, no_std synchronization primitives.
//!
//! ## Feature flags
//!
//! - `os` (on by default): exposes operating system primitives which implement blocking functionality.
#![no_std]

mod event;
mod mutex;

#[doc(inline)]
pub use self::event::Event;

#[doc(inline)]
pub use self::mutex::{RawMutex, RawMutexGuard};

#[cfg(feature = "os")]
#[doc(inline)]
pub use self::event::OsEvent;

#[cfg(feature = "os")]
pub type Mutex<T> = RawMutex<T, OsEvent>;

#[cfg(feature = "os")]
#[doc(inline)]
pub type MutexGuard<'a, T> = RawMutexGuard<'a, T, OsEvent>;
