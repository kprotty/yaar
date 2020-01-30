//! `yaar_lock` aims to provide `#![no_std]` tools for synchronizing resource access across threads and futures.
//!
//! # Features
//! By default, `yaar_lock` provides only the central building block traits and components have to be enabled explicitly.
//!
//! * `os`: Enables [`OsThreadEvent`] and exposes flavors of types which use it. This assumes the platform and provides interfaces to interact with it, similar to libstd.
//! * `sync`: Enables the [`sync`] module which exposes the synchronization primitives that use OS thread blocking.
//! * `future`: Enables the [`future`] module which exposes synchronization primitives that are future-aware.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(feature = "nightly", feature(doc_cfg))]

mod thread_event;
pub use self::thread_event::*;

/// Synchronization primitives based on OS thread blocking
#[cfg(feature = "sync")]
pub mod sync;

/// Synchronization primitives based on futures.
#[cfg(feature = "future")]
pub mod future;
