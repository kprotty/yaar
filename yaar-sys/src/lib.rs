#![no_std]
#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]

#[cfg_attr(windows, path = "./windows.rs")]
mod definitions;

pub use definitions::*;
