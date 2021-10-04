#![warn(
    // missing_debug_implementations,
    // missing_docs,
    rust_2018_idioms,
    unreachable_pub
)]

#[cfg(feature = "rt")]
pub mod runtime;

#[cfg(feature = "rt")]
pub mod task;

#[cfg(feature = "io")]
pub mod io;

#[cfg(feature = "net")]
pub mod net;

#[cfg(feature = "sync")]
pub mod sync;
