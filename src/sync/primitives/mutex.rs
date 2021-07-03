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

use crate::sync::{
    atomic::{AtomicU8, Ordering},
    parker::{block_with, Parker},
    parking_lot::{AsParkingLot, ParkToken, Parked},
};
use core::{cell::Cell, convert::TryInto, marker::PhantomPinned, pin::Pin, time::Duration};

const TOKEN_RETRY: ParkToken = ParkToken(0);
const TOKEN_ACQUIRED: ParkToken = ParkToken(1);

struct Waiter {
    _pinned: PhantomPinned,
    be_fair: fn(Pin<&Self>, Duration) -> bool,
}

const UNLOCKED: u8 = 0;
const LOCKED: u8 = 1;
const PARKED: u8 = 2;

pub struct RawMutex<A> {
    state: AtomicU8,
    parking_lot_provider: A,
}

unsafe impl<A: Send> Send for RawMutex<A> {}
unsafe impl<A: Send> Sync for RawMutex<A> {}

impl<A> RawMutex<A> {
    pub const fn new(parking_lot_provider: A) -> Self {
        Self {
            state: AtomicU8::new(UNLOCKED),
            parking_lot_provider,
        }
    }

    #[inline]
    pub fn try_lock(&self) -> bool {
        self.state
            .compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }
}

impl<A: AsParkingLot> RawMutex<A> {
    #[inline]
    fn lock_fast(&self) -> bool {
        self.state
            .compare_exchange_weak(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    #[inline]
    pub fn lock<P: Parker>(&self) {
        if !self.lock_fast() {
            self.lock_slow::<P, _>(|| None);
        }
    }

    #[inline]
    pub fn try_lock_for<P: Parker>(&self, duration: Duration) -> bool {
        self.lock_fast() || self.lock_slow::<P, _>(|| Some(P::now() + duration))
    }

    #[inline]
    pub fn try_lock_until<P: Parker>(&self, deadline: P::Instant) -> bool {
        self.lock_fast() || self.lock_slow::<P, _>(|| Some(deadline))
    }

    #[cold]
    fn lock_slow<P, D>(&self, deadline_provider: D) -> bool
    where
        P: Parker,
        D: FnOnce() -> Option<P::Instant>,
    {
        unsafe {
            let deadline = deadline_provider();
            let future = self.lock_async::<P>();
            block_with::<P, _>(deadline.as_ref(), future).is_ok()
        }
    }

    pub async fn lock_async<P: Parker>(&self) {
        struct ParkWaiter<P: Parker> {
            started: Cell<Option<P::Instant>>,
            waiter: Waiter,
        }

        impl<P: Parker> ParkWaiter<P> {
            fn be_fair(waiter: Pin<&Waiter>, fair_timeout: Duration) -> bool {
                unsafe {
                    let park_waiter = crate::container_of!(&*waiter, ParkWaiter<P>, waiter);
                    let park_waiter = Pin::new_unchecked(&*park_waiter);

                    let now = P::now();
                    let started = park_waiter.started.replace(None).unwrap();

                    let park_time = now - started.clone();
                    park_time >= fair_timeout || {
                        park_waiter.started.set(Some(started));
                        false
                    }
                }
            }
        }

        let park_waiter = ParkWaiter::<P> {
            started: Cell::new(None),
            waiter: Waiter {
                _pinned: PhantomPinned,
                be_fair: ParkWaiter::<P>::be_fair,
            },
        };

        let mut spin = 0;
        let mut lock_state = LOCKED;
        let mut state = self.state.load(Ordering::Relaxed);
        let park_waiter = unsafe { Pin::new_unchecked(&park_waiter) };

        loop {
            if state == UNLOCKED {
                match self.state.compare_exchange_weak(
                    state,
                    lock_state,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(e) => state = e,
                }
                continue;
            }

            if state != PARKED {
                if P::yield_now(spin) {
                    spin = spin.wrapping_add(1);
                    state = self.state.load(Ordering::Relaxed);
                    continue;
                }

                match self.state.compare_exchange_weak(
                    state,
                    PARKED,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {}
                    Err(e) => {
                        state = e;
                        continue;
                    }
                }
            }

            let started = park_waiter.started.replace(None);
            park_waiter.started.set(match started {
                Some(started) => Some(started),
                None => Some(P::now()),
            });

            let address = self as *const _ as usize;
            let validate = || match self.state.load(Ordering::Relaxed) {
                PARKED => Some(ParkToken(&park_waiter.waiter as *const _ as usize)),
                _ => None,
            };
            let before_park = || {};
            let timed_out = |parked: Parked| {
                if lock_state == PARKED && parked.is_last {
                    let _ = self.state.compare_exchange(
                        PARKED,
                        LOCKED,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    );
                }
            };

            if let Ok(TOKEN_ACQUIRED) = unsafe {
                self.parking_lot_provider
                    .as_parking_lot(address)
                    .park::<P, _, _, _>(address, validate, before_park, timed_out)
                    .await
            } {
                return;
            }

            spin = 0;
            lock_state = PARKED;
            state = self.state.load(Ordering::Relaxed);
        }
    }

    #[inline]
    pub unsafe fn unlock<P: Parker>(&self) {
        self.unlock_fast::<P>(false)
    }

    #[inline]
    pub unsafe fn unlock_fair<P: Parker>(&self) {
        self.unlock_fast::<P>(true)
    }

    #[inline]
    unsafe fn unlock_fast<P: Parker>(&self, be_fair: bool) {
        match self
            .state
            .compare_exchange(LOCKED, UNLOCKED, Ordering::Release, Ordering::Relaxed)
        {
            Ok(_) => {}
            Err(_) => self.unlock_slow::<P>(be_fair),
        }
    }

    #[cold]
    unsafe fn unlock_slow<P: Parker>(&self, be_fair: bool) {
        let address = self as *const _ as usize;
        let on_unpark = |parked: Option<Parked>| {
            if let Some(parked) = parked {
                let waiter = unsafe {
                    let waiter = &*(parked.token.0 as *const Waiter);
                    Pin::new_unchecked(waiter)
                };

                let be_fair = be_fair
                    || (waiter.be_fair)(waiter, {
                        let mut fair_timeout_ns: usize =
                            Duration::from_millis(1).as_nanos().try_into().unwrap();

                        fair_timeout_ns = parked.random % fair_timeout_ns;
                        Duration::from_nanos(fair_timeout_ns as u64)
                    });

                if be_fair {
                    return TOKEN_ACQUIRED;
                }
            }

            self.state.store(UNLOCKED, Ordering::Release);
            TOKEN_RETRY
        };

        self.parking_lot_provider
            .as_parking_lot(address)
            .unpark_one::<P, _>(address, on_unpark)
    }
}
