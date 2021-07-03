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

use crate::sync::parker::Parker;
use core::{pin::Pin, hint::spin_loop, cell::{Cell, UnsafeCell}, ptr::null_mut, convert::TryInto};

type BOOL = u8;
type DWORD = u32;
const INFINITE: DWORD = !0;

type SRWLOCK = *mut ();
const SRWLOCK_INIT: SRWLOCK = null_mut::<()>();

type CONDITION_VARIABLE = *mut ();
const CONDITION_VARIABLE_INIT: SRWLOCK = null_mut::<()>();

#[link(name = "kernel32")]
extern "system" {
    fn AcquireSRWLockExclusive(lock: *mut SRWLOCK);
    fn ReleaseSRWLockExclusive(lock: *mut SRWLOCK);

    fn WakeConditionVariable(c: *mut CONDITION_VARIABLE);
    fn SleepConditionVariableSRW(
        cond: *mut CONDITION_VARIABLE,
        lock: *mut SRWLOCK,
        timeout_ms: DWORD,
        flags: DWORD,
    ) -> BOOL;
}

#[derive(Default)]
pub struct OsParker {
    notified: Cell<bool>,
    lock: UnsafeCell<SRWLOCK>,
    cond: UnsafeCell<CONDITION_VARIABLE>,
}

impl OsParker {
    pub const fn new() -> Self {
        Self {
            notified: AtomicBool::new(false),
            lock: UnsafeCell::new(SRWLOCK_INIT),
            cond: UnsafeCell::new(CONDITION_VARIABLE_INIT),
        }
    }

    fn with_lock<T>(&self, f: impl FnOnce() -> T) -> T {
        unsafe {
            AcquireSRWLockExclusive(self.lock.get());
            let result = f();
            ReleaseSRWLockExclusive(self.lock.get());
            result
        }
    }
}

unsafe impl Parker for OsParker {
    type Instant = super::instant::OsInstant;

    fn now() -> Self::Instant {
        Self::Instant::now();
    }

    fn yield_now(iteration: usize) -> bool {
        if iteration < 1000 {
            spin_loop();
            true
        } else {
            false
        }
    }

    fn prepare(self: Pin<&Self>) {
        self.notified.set(false);
    }

    fn park(self: Pin<&Self>, deadline: Option<Self::Instant>) -> bool {
        self.with_lock(|| unsafe {
            loop {
                if self.notified.get() {
                    return true;
                }

                let mut timeout_ms = INFINITE;
                if let Some(deadline) = deadline {
                    timeout_ms = match deadline.checked_duration_since(Self::now()) {
                        None => return false,
                        Some(duration) => duration.as_millis().try_into().unwrap(),
                    };
                }

                _ = SleepConditionVariableSRW(
                    self.cond.get(),
                    self.lock.get(),
                    timeout_ms,
                    0,
                );
            }
        })
    }

    fn unpark(self: Pin<&Self>) {
        self.with_lock(|| unsafe {
            self.notified.set(true);
            WakeConditionVariable(self.cond.get());
        })
    }
}