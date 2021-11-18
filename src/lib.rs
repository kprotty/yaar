#![forbid(unsafe_code)]

mod waker;

pub use tokio::io;

#[cfg(feature = "net")]
pub mod net;

#[cfg(feature = "time")]
pub mod time;

#[cfg(feature = "rt")]
pub mod runtime;

#[cfg(feature = "sync")]
pub mod sync;