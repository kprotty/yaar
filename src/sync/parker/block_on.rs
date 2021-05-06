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
use core::{
    future::Future,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
    time::Duration,
};

pub unsafe fn block_on<P: Parker, T>(
    deadline: Option<&P::Instant>,
    mut future: impl Future<Output = T>,
) -> Result<T, ()> {
    struct ParkWaker<P>(PhantomData<*mut P>);

    impl<P: Parker> ParkWaker<P> {
        pub const VTABLE: RawWakerVTable = RawWakerVTable::new(
            |ptr| unsafe {
                let parker = Pin::new_unchecked(&*(ptr as *const P));
                parker.prepare();
                RawWaker::new(ptr, &Self::VTABLE)
            },
            |ptr| unsafe {
                let parker = Pin::new_unchecked(&*(ptr as *const P));
                parker.unpark();
            },
            |_ptr| unreachable!("block_on<Parker>(): wake_by_ref()"),
            |_ptr| {},
        );
    }

    let parker = P::default();
    let parker = Pin::new_unchecked(&parker);
    let waker = Waker::from_raw({
        let ptr = &*parker as *const P as *const ();
        RawWaker::new(ptr, &ParkWaker::<P>::VTABLE)
    });

    loop {
        let future = Pin::new_unchecked(&mut future);
        let mut context = Context::from_waker(&waker);

        if let Poll::Ready(value) = future.poll(&mut context) {
            return Ok(value);
        }

        let timed_out = !parker.park(deadline);
        if timed_out {
            assert!(deadline.is_some());
            return Err(());
        }
    }
}
