yaar-reactor
[![Crates.io](https://img.shields.io/crates/v/yaar-reactor.svg)](https://crates.io/crates/yaar-reactor)
[![Documentation](https://docs.rs/yaar-reactor/badge.svg)](https://docs.rs/yaar-reactor/)
====

Non-blocking IO abstractions for building executors. 
This crate is currently under development.

## Usage
Add this to your `Cargo.toml`:
```toml
[dependencies]
yaar-reactor = { version = "0.1", features = ["full"] }
```

`yaar-reactor` has features enabled by defualt and requires them to be set explicitely.
As a shorthand, the `full` feature enables all components.
See the [Documentation](https://docs.rs/yaar-reactor/) for more on the available features.

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