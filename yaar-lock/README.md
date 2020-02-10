yaar-lock
[![Crates.io](https://img.shields.io/crates/v/yaar-lock.svg)](https://crates.io/crates/yaar-lock)
[![Documentation](https://docs.rs/yaar-lock/badge.svg)](https://docs.rs/yaar-lock/)
====

Small & Fast synchronization primitives for `#![no_std]` environments.

`yaar-lock` provides synchronization primitives for both blocking and non-blocking (futures) environments. For blocking primitives, it relies on a [user-supplied] implementation for parking threads while also providing a [default] for common platforms. This method of extensibility with convenient defaults is a pattern that can be seen across other `yaar` components. 

[user-supplied]: https://docs.rs/yaar-lock/0.2.1/yaar_lock/trait.ThreadEvent.html
[default]: https://docs.rs/yaar-lock/0.2.1/yaar_lock/struct.OsThreadEvent.html

## Usage
Add this to your `Cargo.toml`:
```toml
[dependencies]
yaar_lock = { version = "0.2", features = ["full"] }
```

`yaar-lock` has features enabled by defualt and requires them to be set explicitely.
As a shorthand, the `full` feature enables all components.
See the [Documentation](https://docs.rs/yaar-lock/) for more on the available features.

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