//#![cfg_attr(not(test), no_std)]
#![warn(rust_2018_idioms)]
#![cfg_attr(feature = "nightly", feature(doc_cfg))]

pub mod utils;

pub mod event;

#[cfg(feature = "sync")]
pub mod sync;

#[cfg(feature = "future")]
pub mod future;
