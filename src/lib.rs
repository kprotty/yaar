#![forbid(unsafe_code)]

mod dependencies;

#[cfg(feature = "io")]
pub use tokio::io;

#[cfg(feature = "rt")]
pub mod runtime;
