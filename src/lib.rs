#![forbid(unsafe_code)]

pub mod io;
pub mod net;
pub mod runtime;
pub mod sync;

pub use task::spawn;
pub mod task {
    pub use crate::runtime::executor::task::{spawn, yield_now, JoinHandle};
}
