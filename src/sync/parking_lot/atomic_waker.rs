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

use crate::sync::atomic::{fence, AtomicUsize, Ordering};
use core::{
    cell::Cell,
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};

const WAKER_EMPTY: usize = 0;
const WAKER_WAITING: usize = 1;
const WAKER_UPDATING: usize = 2;
const WAKER_NOTIFIED: usize = 3;

#[derive(Default)]
pub struct AtomicWaker {
    state: AtomicUsize,
    waker: Cell<Option<Waker>>,
}

unsafe impl Send for AtomicWaker {}
unsafe impl Sync for AtomicWaker {}

impl AtomicWaker {
    pub unsafe fn wait(&self) -> AtomicWakerFuture<'_> {
        AtomicWakerFuture(self)
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "riscv64"))]
    pub unsafe fn wake(&self) {
        fence(Ordering::Release);
        if self.notify() {
            self.waker.take().expect("waiting without a waker").wake();
        }
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "riscv64"))]
    fn notify(&self) -> bool {
        let state = self.state.swap(WAKER_NOTIFIED, Ordering::Acquire);
        state == WAKER_WAITING
    }

    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "riscv64")))]
    fn notify(&self) -> bool {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            let notify_ordering = match state {
                WAKER_EMPTY => Ordering::Relaxed,
                WAKER_WAITING => Ordering::Acquire,
                WAKER_UPDATING => Ordering::Relaxed,
                _ => unreachable!("invalid waker state"),
            };

            match self.state.compare_exchange_weak(
                state,
                WAKER_NOTIFIED,
                notify_ordering,
                Ordering::Relaxed,
            ) {
                Ok(WAKER_WAITING) => return true,
                Ok(_) => return false,
                Err(e) => state = e,
            }
        }
    }
}

pub struct AtomicWakerFuture<'a>(&'a AtomicWaker);

impl<'a> Future for AtomicWakerFuture<'a> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let result = match self.0.state.load(Ordering::Relaxed) {
            WAKER_EMPTY => {
                self.0.waker.set(Some(ctx.waker().clone()));
                match self.0.state.compare_exchange(
                    WAKER_EMPTY,
                    WAKER_WAITING,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => Poll::Pending,
                    Err(WAKER_NOTIFIED) => Poll::Ready(()),
                    _ => unreachable!("invalid atomic waker state"),
                }
            }
            WAKER_WAITING => {
                // Acquire barrier:
                // - keeps waker.set() from being reordered before the transition to WAKER_UPDATING
                // - observes valid waker.will_wake() if being polled from another thread
                match self.0.state.compare_exchange(
                    WAKER_WAITING,
                    WAKER_UPDATING,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {}
                    Err(WAKER_NOTIFIED) => return Poll::Ready(()),
                    Err(_) => unreachable!(),
                }

                let mut waker = self.0.waker.replace(None).expect("updating without waker");
                if !ctx.waker().will_wake(&waker) {
                    waker = ctx.waker().clone();
                }
                self.0.waker.set(Some(waker));

                // Release barrier:
                // - keeps waker.set() from being reordered after the transition back to WAKER_WAITING
                // - makes the waker.set() visible to future poll() threads and AtomicWaker::wake() threads
                match self.0.state.compare_exchange(
                    WAKER_UPDATING,
                    WAKER_WAITING,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => Poll::Pending,
                    Err(WAKER_NOTIFIED) => Poll::Ready(()),
                    Err(_) => unreachable!(),
                }
            }
            WAKER_UPDATING => unreachable!("polled when currently being polled"),
            WAKER_NOTIFIED => Poll::Ready(()),
            _ => unreachable!("invalid atomic waker state"),
        };

        if result != Poll::Pending {
            fence(Ordering::Acquire);
        }

        result
    }
}
