pub use self::sync_impl::{Condvar, Mutex, MutexGuard};

#[cfg(feature = "parking_lot")]
mod sync_impl {}

#[cfg(not(feature = "parking_lot"))]
mod sync_impl {}
