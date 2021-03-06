yaar
[![Crates.io](https://img.shields.io/crates/v/yaar.svg)](https://crates.io/crates/yaar)
[![Documentation](https://docs.rs/yaar/badge.svg)](https://docs.rs/yaar/)
====

**Y**et **A**nother **A**synchronous **R**untime (yaar)
optimized around configuration and no_std. This crate is currently under development.

## Overview

Yaar is a customizable runtime built around the idea of intrusive data structures and resource efficiency. Some of the features it provides include:

* Multithreaded, NUMA-aware, work-stealing task scheduler (not limited to futures).
* Extendable primitives for swapping in and implementing your own task scheduler.
* (soon) Utilities for efficient future exection and structured concurrency.

## Usage
Add this to your `Cargo.toml`:
```toml
[dependencies]
yaar = { version = "0.1", features = ["full"] }
```

`yaar` has features enabled by defualt and requires them to be set explicitely.
As a shorthand, the `full` feature enables all components.
See the [Documentation](https://docs.rs/yaar/) for more on the available features.

## License

Licensed under either of

 * Apache License, Version 2.0
   ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license
   ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.