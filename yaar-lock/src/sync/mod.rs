//! Collection of synchronization primitives which
//! support blocking the current OS thread.

mod mutex;
mod thread_parker;

#[doc(inline)]
pub use self::mutex::*;

#[doc(inline)]
pub use self::thread_parker::*;
