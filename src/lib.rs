#![warn(
    // missing_debug_implementations,
    // missing_docs,
    rust_2018_idioms,
    unreachable_pub
)]

#[cfg(feature = "net")]
mod io;
#[cfg(feature = "rt")]
mod runtime;

#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "sync")]
pub mod sync;
#[cfg(feature = "rt")]
pub mod task;
