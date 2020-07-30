//! A collection of libraries for writing configurable,
//! performant, and resource efficient systems.
//!
//! Yaar provides to the user synchronization primitives,
//! a general purpose task scheduler, and an asynchronous runtime
//! to power many concurrent or parallel tasks from threaded processing to I/O.
//!
//! This crate is still a work-in-progress with many exciting changes yet to come.
//!
//!

#![no_std]
#![warn(
    missing_docs,
    unreachable_pub,
    rust_2018_idioms,
    missing_debug_implementations,
)]
#![cfg_attr(
    feature = "nightly",
    feature(
        doc_cfg,
        target_has_atomic,
    ),
)]