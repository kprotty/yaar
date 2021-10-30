#![forbid(unsafe_code)]

pub mod io;
pub mod runtime;
pub mod sync;

pub mod task {
    pub use crate::runtime::task::{spawn, yield_now, JoinHandle};
}
