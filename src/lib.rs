#![no_std]

#[cfg(all(unix, feature = "os"))]
extern crate libc;

#[cfg(all(windows, feature = "os"))]
extern crate winapi;

#[cfg(feature = "lock")]
extern crate lock_api;

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "lock")]
pub mod lock;

#[cfg(feature = "rt")]
pub mod runtime;

#[cfg(feature = "io")]
pub mod io;
