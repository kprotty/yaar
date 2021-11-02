#![forbid(unsafe_code)]

pub mod io;
pub mod net;
pub mod runtime;
pub mod sync;

pub mod task;
pub use task::spawn;
