#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(unix)]
extern crate libc;
#[cfg(windows)]
extern crate winapi;

pub mod runtime;
