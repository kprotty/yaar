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

mod atomic_waker;
mod lock;
mod parking_lot;
mod queue;

use core::mem::size_of;
pub use parking_lot::{ParkToken, Parked, ParkingLot, Unparked};

pub trait AsParkingLot {
    fn as_parking_lot(&self, address: usize) -> &ParkingLot;
}

impl AsParkingLot for ParkingLot {
    fn as_parking_lot(&self, _address: usize) -> &ParkingLot {
        self
    }
}

impl<const N: usize> AsParkingLot for [ParkingLot; N] {
    fn as_parking_lot(&self, address: usize) -> &ParkingLot {
        hashed_parking_lot(self, address)
    }
}

impl<'a> AsParkingLot for &'a [ParkingLot] {
    fn as_parking_lot(&self, address: usize) -> &ParkingLot {
        hashed_parking_lot(self, address)
    }
}

#[inline]
fn hashed_parking_lot(slice: &[ParkingLot], address: usize) -> &ParkingLot {
    let seed = match size_of::<usize>() {
        8 => 0x9E3779B97F4A7C15,
        4 => 0x9E3779B9,
        2 => 0x9E37,
        _ => unreachable!("Architecture not supported"),
    };

    let hash = address.wrapping_mul(seed);
    let index = hash % slice.len();
    &slice[index]
}
