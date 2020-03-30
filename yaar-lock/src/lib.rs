#![cfg_attr(not(test), no_std)]
#![cfg_attr(feature = "nightly", feature(doc_cfg))]

pub mod utils;
pub mod parker;

pub(crate) mod shared;

#[cfg(feature = "sync")]
pub mod sync;

#[cfg(feature = "future")]
pub mod future;