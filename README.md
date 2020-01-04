# yaar
Yet another asynchronous runtime optimized around configuration and no_std.

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
