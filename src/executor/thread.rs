// Copyright 2019-2020 kprotty
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use core::{fmt, marker::PhantomPinned};
use super::LocalQueue;

/// TODO: documentation for a Thread
pub struct Thread {
    _pinned: PhantomPinned,
    run_queue: LocalQueue,
}

impl fmt::Debug for Thread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Thread").finish()
    }
}

impl Default for Thread {
    fn default() -> Self {
        Self {
            _pinned: PhantomPinned,
            run_queue: LocalQueue::default(),
        }
    }
}
