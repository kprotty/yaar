yaar
[![Crates.io](https://img.shields.io/crates/v/yaar.svg)](https://crates.io/crates/yaar)
[![Documentation](https://docs.rs/yaar/badge.svg)](https://docs.rs/yaar/)
====

**Y**et **A**nother **A**synchronous **R**untime (yaar) focused on `#![forbid(unsafe_code)]` and scalability. 

***This crate is currently under development***.

## Overview:
I decided to challenge myself in writing an async runtime for Rust that doesn't use unsafe. The rules were just that, so I can still rely on other unsafe crates (i.e. parking_lot, crossbeam, arc-swap). The goal is to make a `#![forbid(unsafe_code)]` that's competitive in performance with [tokio](https://github.com/tokio-rs/tokio/). **This is a research project first, and a library second.** (At least for now)

## License

This project is licensed under the MIT license.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the MIT license, shall be licensed as above, without any additional terms or conditions.