#![cfg_attr(not(test), no_std)]
#![cfg_attr(feature = "nightly", feature(doc_cfg))]

mod signal;
pub use signal::*;

#[cfg(feature = "sync")]
pub mod sync;

#[cfg(feature = "future")]
pub mod future;
