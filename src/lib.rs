#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(all(feature = "platform-os", unix))]
extern crate libc;

#[cfg(all(feature = "platform-os", windows))]
extern crate winapi;

pub mod runtime;
