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
    atomic::{AtomicBool, Ordering},
    parker::{block_with, Parker},
    parking_lot::{AsParkingLot, ParkToken, Unparked},
    primitives::MutexGuard,
};
use core::{marker::PhantomData, time::Duration};

#[derive(Default, Debug)]
pub struct Condvar<A, P> {
    parking_lot_provider: A,
    has_waiters: AtomicBool,
    _parker: PhantomData<P>,
}

impl<A, P> Condvar<A, P> {
    pub const fn new(parking_lot_provider: A) -> Self {
        Self {
            parking_lot_provider,
            has_waiters: AtomicBool::new(false),
            _parker: PhantomData,
        }
    }
}

impl<A: AsParkingLot, P: Parker> Condvar<A, P> {
    pub fn wait<MutexA: AsParkingLot, T>(&self, guard: &mut MutexGuard<MutexA, P, T>) {
        self.wait_with(guard, || None);
    }

    pub fn wait_for<MutexA: AsParkingLot, T>(
        &self,
        guard: &mut MutexGuard<MutexA, P, T>,
        duration: Duration,
    ) -> Option<()> {
        self.wait_with(guard, || Some(P::now() + duration))
    }

    pub fn wait_until<MutexA: AsParkingLot, T>(
        &self,
        guard: &mut MutexGuard<MutexA, P, T>,
        deadline: P::Instant,
    ) -> Option<()> {
        self.wait_with(guard, || Some(deadline))
    }

    fn wait_with<MutexA: AsParkingLot, T>(
        &self,
        guard: &mut MutexGuard<MutexA, P, T>,
        deadline_provider: impl FnOnce() -> Option<P::Instant>,
    ) -> Option<()> {
        unsafe {
            let deadline = deadline_provider();
            let future = self.wait_async(guard);
            block_with::<P, _>(deadline, future).ok()
        }
    }

    pub async fn wait_async<MutexA: AsParkingLot, T>(
        &self,
        guard: &mut MutexGuard<'_, MutexA, P, T>,
    ) {
        let address = self as *const _ as usize;
        let validate = || {
            self.has_waiters.store(true, Ordering::Relaxed);
            Some(ParkToken(0))
        };
        let before_park = || unsafe { guard.raw.unlock::<P>() };
        let timed_out = |_parked| {};

        unsafe {
            self.parking_lot_provider
                .as_parking_lot(address)
                .park::<P, _, _, _>(address, validate, before_park, timed_out)
                .await;
        }
    }

    #[inline]
    pub fn notify_one(&self) -> bool {
        if self.has_waiters.load(Ordering::Relaxed) {
            self.notify_one_slow()
        } else {
            false
        }
    }

    #[cold]
    fn notify_one_slow(&self) -> bool {
        let mut did_unpark = false;
        let address = self as *const _ as usize;

        unsafe {
            self.parking_lot_provider
                .as_parking_lot(address)
                .unpark_one::<P, _>(address, |parked| {
                    did_unpark = parked.is_some();
                    if parked.map(|p| p.is_last).unwrap_or(true) {
                        self.has_waiters.store(false, Ordering::Relaxed);
                    }
                    ParkToken(0)
                });
        }

        did_unpark
    }

    #[inline]
    pub fn notify_all(&self) -> usize {
        if self.has_waiters.load(Ordering::Relaxed) {
            self.notify_all_slow()
        } else {
            0
        }
    }

    #[cold]
    fn notify_all_slow(&self) -> usize {
        let mut unparked = 0;
        let address = self as *const _ as usize;

        let filter = |_parked| {
            unparked += 1;
            Unparked::Unpark(ParkToken(0))
        };

        unsafe {
            self.has_waiters.store(false, Ordering::Relaxed);
            self.parking_lot_provider
                .as_parking_lot(address)
                .unpark::<P, _, _>(address, filter, || {});
        }

        unparked
    }
}
