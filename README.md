yaar
[![Crates.io](https://img.shields.io/crates/v/yaar.svg)](https://crates.io/crates/yaar)
[![Documentation](https://docs.rs/yaar/badge.svg)](https://docs.rs/yaar/)
====

**Y**et **A**nother **A**synchronous **R**untime (yaar) optimized around configuration and no_std.

***This crate is currently under development***.

## Overview:

`yaar` aims to provide both a multi-threaded and single-threadead task scheduler which can be used to executing futures for the `no_std` ecosystem. It does this by using intrusively provided structures and requiring traits for interaction with aspects of the runtime that would otherwise be platform depedent (i.e. Threading, TLS, IO, Timers, etc.). It's components are currently split between a few crates for modularity:

* [`yaar`]: A configurable runtime/task scheduler built around intrusive data structures and (soon) futures.
* [`yaar-lock`]: Synchronization primitives for both blocking and futures based environments.
* [`yaar-reactor`]: Generic traits for implementing non-blocking IO focused on futures.

[`yaar`]: https://github.com/kprotty/yaar/tree/master/yaar
[`yaar-lock`]: https://github.com/kprotty/yaar/tree/master/yaar-lock
[`yaar-reactor`]: https://github.com/kprotty/yaar/tree/master/yaar-reactor

## Usage
Add this to your `Cargo.toml`:
```toml
[dependencies]
yaar = { version = "0.1", features = ["full"] }
```

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