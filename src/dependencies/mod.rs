#[cfg(feature = "loom")]
pub use loom;
#[cfg(not(feature = "loom"))]
pub mod loom;

#[cfg(feature = "try_lock")]
pub use try_lock;
#[cfg(not(feature = "try_lock"))]
pub mod try_lock;

#[cfg(feature = "parking_lot")]
pub use parking_lot;
#[cfg(not(feature = "parking_lot"))]
pub mod parking_lot;

#[cfg(feature = "crossbeam_deque")]
pub use crossbeam_deque;
#[cfg(not(feature = "crossbeam_deque"))]
pub mod crossbeam_deque;
