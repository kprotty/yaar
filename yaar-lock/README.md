yaar-lock
[![Licence](https://img.shields.io/badge/license-MIT%20or%20Apache-blue.svg)](#License)
[![Documentation](https://docs.rs/yaar-lock/badge.svg)](https://docs.rs/yaar-lock/)
====

This library currently provides Mutex implementations which are based on parking_lot
making them small (only a `usize` large), fast, and configurable in `#![no_std]`
environments.

## Usage
Add this to your `Cargo.toml`:
```toml
[dependencies]
yaar_lock = "0.1"
```

To disable the default provided OS implements for `#![no_std]` 
provided by the `os` feature, set the default features manually:
```toml
[dependencies]
yaar_lock = { version = "0.1", default-features = false }
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