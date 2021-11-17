#![forbid(unsafe_code)]

mod futex;
mod mutex;

pub use waker::AtomicWaker;
pub use mutex::{Mutex, MutexGuard};