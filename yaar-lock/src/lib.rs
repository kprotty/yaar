//! Fast, no_std synchronization primitives.
//!
//! ## Feature flags
//! All features are on by default.
//!
//! - `os`: exposes operating system primitives which implement thread parking.
//! - `sync`: exposes synchronization primitives backed by thread parking. 
//! - `async`: exposes synchronization primitives backed by futures.
#![no_std]

mod mutex;
mod thread_parker;

pub use self::mutex::*;
pub use self::thread_parker::*;
