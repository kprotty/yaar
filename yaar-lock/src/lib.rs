#![cfg_attr(not(test), no_std)]
#![cfg_attr(feature = "nightly", feature(doc_cfg))]

mod thread_event;
pub use self::thread_event::*;

/// Synchronization primitives based on OS thread blocking
#[cfg(feature = "sync")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "sync")))]
pub mod sync;

/// Synchronization primitives based on futures.
#[cfg(feature = "future")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "future")))]
pub mod future;
