#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(
    // missing_debug_implementations,
    // missing_docs,
    rust_2018_idioms,
    // unreachable_pub
)]

mod internal;

#[cfg(feature = "rt")]
pub mod runtime;

#[cfg(feature = "sync")]
pub mod sync;

#[cfg(feature = "io")]
pub mod io;

#[cfg(feature = "net")]
pub mod net;

#[cfg(feature = "fs")]
pub mod fs;

#[cfg(feature = "time")]
pub mod time;