<h1 align="center">yaar</h1>
<div align="center">
    <strong>
        Yet another asynchronous runtime optimized around configuration and no_std.
    </strong>
</div>

<br />

[![Licence](https://img.shields.io/badge/license-MIT%20or%20Apache-blue.svg)](#License)
[![Version](https://img.shields.io/badge/version-alpha-red.svg)](#License)

At the time of writing, the current rust ecosystem appears to expose work stealing
async Future executors only built around std and rust allocation semantics. This
library serves as an experiment to circumvent these requirements by providing
Future executors and libraries built around them with the principle that
components should be customizable while aiming to not increase runtime overhead.
This is explored primarily in two ways:

- Expose a method to customize the memory allocator used by the runtime instead
of relying on `core::global_allocator` while at the same time striving to
only allocate memory when truly required and in bulk for specialized allocation.

- Expose a method of providing custom implementations for platform or program
specific functionality such as threading, io, and memory in order to be truly 
platform agnostic and customizable.

These goals should allow `yaar` to support execution in `#![no_std]` environments
as well as allow new platforms to integrate into whatever code is built on top of it.

## Features

* `std`: by default, `yaar` links to the standard library for easy access to an allocator
(`yaar::runtime::platform::StdAllocator`). Disable this to allow use in `#![no_std]` crates.

* `os-platform`: by default, `yaar` provides a platform implementation for common operating
systems including Windows, Linux, Mac and other BSD variants (`yaar::runtime::platform::OsPlatform`).

## License

<sup>
Licensed under either of <a href="LICENSE-APACHE">Apache License, Version
2.0</a> or <a href="LICENSE-MIT">MIT license</a> at your option.
</sup>

<br/>

<sub>
Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
</sub>