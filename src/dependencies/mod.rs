#[cfg(feature = "crossbeam-deque")]
pub use crossbeam_deque;
#[cfg(not(feature = "crossbeam-deque"))]
pub mod crossbeam_deque;

#[cfg(feature = "parking_lot")]
pub use parking_lot;
#[cfg(not(feature = "parking_lot"))]
pub mod parking_lot;

