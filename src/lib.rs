
#[cfg(unix)]
extern crate libc;
#[cfg(windows)]
extern crate winapi;

extern crate num_cpus;
extern crate crossbeam;

pub mod runtime;
