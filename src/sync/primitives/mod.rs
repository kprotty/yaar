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

mod condvar;
mod mutex;

pub use condvar::Condvar;
pub use mutex::{Mutex, MutexGuard, RawMutex};

#[cfg(feature = "os")]
pub use os_primitives;

#[cfg(feature = "os")]
mod os_primitives {
    use super::*;
    use crate::sync::{
        parker::OsParker,
        parking_lot::OsParkingLot,
    };

    pub type OsRawMutex = RawMutex<OsParkingLot>;
    pub type OsMutex<T> = Mutex<OsParkingLot, OsParker, T>;
    pub type OsMutexGuard<'a, T> = MutexGuard<'a, OsParkingLot, OsParker, T>;

    pub const fn make_os_mutex<T>(value: T) -> OsMutex<T> {
        OsMutex::new(OsParkingLot{}, value)
    }

    pub type OsCondvar = Condvar<OsParkingLot, OsParker>;

    pub const fn make_os_condvar() -> OsCondvar {
        OsCondvar::new(OsParkingLot{})
    }
}