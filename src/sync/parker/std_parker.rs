// Copyright (c) 2021 kprotty
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License

use super::Parker;
use std::{
    cell::Cell,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    thread::{self, Thread},
};

#[derive(Default)]
pub struct StdParker {
    notified: AtomicBool,
    thread: Cell<Option<Thread>>,
}

unsafe impl Send for StdParker {}
unsafe impl Sync for StdParker {}

impl StdParker {
    pub const fn new() -> Self {
        Self {
            notified: AtomicBool::new(false),
            thread: Cell::new(None),
        }
    }
}

unsafe impl Parker for StdParker {
    type Instant = std::time::Instant;

    fn now() -> Self::Instant {
        Self::Instant::now()
    }

    fn yield_now(iteration: usize) -> bool {
        if iteration <= 3 {
            (0..(1 << iteration)).for_each(|_| std::hint::spin_loop());
            true
        } else if iteration <= 10 {
            std::thread::yield_now();
            true
        } else {
            false
        }
    }

    fn prepare(self: Pin<&Self>) {
        self.thread.set(Some(thread::current()));
        self.notified.store(false, Ordering::Relaxed);
    }

    fn park(self: Pin<&Self>, deadline: Option<Self::Instant>) -> bool {
        loop {
            if self.notified.load(Ordering::Acquire) {
                return true;
            }

            let deadline = match deadline {
                Some(deadline) => deadline,
                None => {
                    thread::park();
                    continue;
                }
            };

            let now = Self::Instant::now();
            thread::park_timeout(match deadline.checked_duration_since(now) {
                Some(duration) => duration,
                None => return false,
            });
        }
    }

    fn unpark(self: Pin<&Self>) {
        let thread = self.thread.replace(None);
        self.notified.store(true, Ordering::Release);
        thread.expect("StdParker without a thread").unpark();
    }
}
