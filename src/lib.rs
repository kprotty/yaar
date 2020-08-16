#![no_std]
#![warn(rust_2018_idioms)]
#![cfg_attr(feature = "nightly", feature(llvm_asm, doc_cfg))]

#[cfg(feature = "executor")]
#[cfg_attr(feature = "nightly", doc(cfg(feature = "executor")))]
pub mod executor;

pub mod sync;
