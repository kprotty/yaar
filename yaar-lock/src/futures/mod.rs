//! Collection of synchronization primitives which
//! support non-blocking integration with futures. 

mod mutex;

#[doc(inline)]
pub use self::mutex::*;
