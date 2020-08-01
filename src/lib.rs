// Copyright 2019-2020 kprotty
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! A collection of libraries for writing configurable,
//! performant, and resource efficient systems.
//!
//! Yaar provides to the user synchronization primitives,
//! a general purpose task scheduler, and an asynchronous runtime
//! to power many concurrent or parallel tasks from threaded processing to I/O.
//!
//! This crate is still a work-in-progress with many exciting changes yet to
//! come.

#![no_std]
#![cfg_attr(feature = "nightly", feature(doc_cfg, const_fn))]
#![warn(
    missing_docs,
    unreachable_pub,
    rust_2018_idioms,
    missing_debug_implementations
)]

#[cfg_attr(feature = "nightly", doc(cfg(feature = "executor")))]
#[cfg(feature = "executor")]
pub mod executor;
