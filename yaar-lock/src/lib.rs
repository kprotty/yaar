#![cfg_attr(not(test), no_std)]
#![cfg_attr(feature = "nightly", feature(doc_cfg))]

mod event;
pub use event::*;

pub mod utils;

pub(crate) mod shared;

#[cfg(feature = "sync")]
pub mod sync;

#[cfg(feature = "future")]
pub mod future;
