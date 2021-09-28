yaar
[![Crates.io](https://img.shields.io/crates/v/yaar.svg)](https://crates.io/crates/yaar)
[![Documentation](https://docs.rs/yaar/badge.svg)](https://docs.rs/yaar/)
====

**Y**et **A**nother **A**synchronous **R**untime (yaar) with the goal of building efficient async primitives.

***This crate is currently under development***.

## Overview:
This started as a port of [Resource Efficient Thread Pools](https://zig.news/kprotty/resource-efficient-thread-pools-with-zig-3291) to Rust async but slowly grew into a proper async runtime. The goal is to explore if and how standard async stuff in Rust can be made more efficient either in memory usage or general scalability. The point of reference is [tokio](https://github.com/tokio-rs/tokio/). **This is a research project first, and a library second.** (At least for now)

## Usage
Add this to your `Cargo.toml`:
```toml
[dependencies]
yaar = { version = "0.1", features = ["full"] }
```

## License

This project is licensed under the MIT license.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.