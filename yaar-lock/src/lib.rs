//! Fast, no_std synchronization primitives.
//!
//! ## Feature flags
//! All features are on by default.
//!
//! - `os`: exposes operating system primitives which implement thread parking.
//! - `sync`: exposes synchronization primitives backed by thread parking.
//! - `futures`: exposes synchronization primitives backed by futures.
#![no_std]

mod shared;

#[cfg(feature = "sync")]
pub mod sync;

#[cfg(feature = "futures")]
pub mod futures;
