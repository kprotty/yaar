#![no_std]
#![cfg_attr(feature = "nightly", feature(doc_cfg, thread_local))]

#[cfg(feature = "sync")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "sync")))]
pub mod sync;
