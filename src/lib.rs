#![forbid(unsafe_code)]

#[cfg(any(feature = "rt", feature = "net", feature = "time", feature = "sync"))]
mod dependencies;

#[cfg(any(feature = "rt", feature = "net", feature = "time", feature = "sync"))]
mod event;

pub use tokio::io;

#[cfg(feature = "rt")]
pub mod runtime;

#[cfg(feature = "net")]
pub mod net;

#[cfg(feature = "fs")]
pub mod fs;

#[cfg(feature = "time")]
pub mod time;

#[cfg(feature = "sync")]
pub mod sync;
