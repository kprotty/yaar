pub mod error;
pub(crate) mod internal;
mod sleeps;
mod timeouts;

pub use sleeps::{sleep, sleep_until, Sleep};
pub use std::time::{Duration, Instant};
pub use timeouts::{timeout, timeout_at, Timeout};
