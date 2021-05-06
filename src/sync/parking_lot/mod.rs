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

mod lock;
mod parking_lot;
mod queue;

use core::mem::size_of;
pub use parking_lot::{ParkToken, Parked, ParkFairness, ParkingLot, Unparked};

pub trait WithParkingLot {
    fn with_parking_lot<F>(&self, address: usize, f: impl FnOnce(&ParkingLot) -> F) -> F;
}

pub struct SharededParkingLot<const N: usize, F> {
    array: [ParkingLot<F>; N],
}

impl<const N: usize, F> SharededParkingLot<N, F> {
    pub const fn new(fairness: F) -> Self {
        const INSTANCE: ParkingLot<F> = ParkingLot::<F>::new(fairness);
        Self {
            array: [INSTANCE; N],
        }
    }
}

impl<const N: usize> WithParkingLot for SharededParkingLot<N> {
    fn with_parking_lot<F>(&self, address: usize, f: impl FnOnce(&ParkingLot) -> F) -> F {
        let seed = match size_of::<usize>() {
            8 => 0x9E3779B97F4A7C15,
            4 => 0x9E3779B9,
            2 => 0x9E37,
            _ => unreachable!("Architecture not supported"),
        };

        f({
            let hash = address.wrapping_mul(seed);
            let index = hash % N;
            &self.array[index]
        })
    }
}
