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

use super::{lock::WaitLock, queue::{WaitNode, WaitQueue, WaitTree}};
use crate::sync::parker::Parker;
use core::{
    pin::Pin,
    cell::Cell,
    ptr::NonNull,
};

pub struct ParkToken(pub usize);

pub enum Unparked {
    Stop,
    Skip,
    Unpark(ParkToken),
}

pub struct Parked {
    token: ParkToken,
    is_last: bool,
    fair_ptr: *const (),
    fair_callback: unsafe fn(*const (), ParkToken) -> bool,
}

impl Parked {
    pub fn token(&self) -> ParkToken {
        self.token
    }

    pub fn is_last(&self) -> bool {
        self.is_last
    }

    pub fn be_fair(&self) -> bool {
        (self.fair_callback)(self.fair_ptr, self.token)
    }
}

pub trait ParkFairness {
    fn be_fair<P: Parker>(
        &self,
        rand: usize,
        token: usize,
    ) -> bool;
}

struct ParkNode {
    wait_node: WaitNode,
    token: Cell<ParkToken>,
    unpark_next: Cell<Option<NonNull<ParkNode>>>,
}

struct ParkInner<F> {
    tree: WaitTree,
    fairness: F,
}

impl<F: ParkFairness> ParkInner<F> {
    fn parked(&self, token: ParkToken, is_last: bool) -> Parked {
        Parked {
            token,
            is_last,
            fair_ptr: self as *const Self as *const (),
            fair_callback: |ptr, token| unsafe {
                let inner = &*(ptr as *const Self);
                let random = inner.tree.gen_random();
                inner.fairness.be_fair(random, token)
            },
        }
    }
}

pub struct ParkingLot<F> {
    inner: WaitLock<ParkInner<F>>,
}

impl<F> ParkingLot<F> {
    pub const fn new(fairness: F) -> Self {
        Self {
            inner: WaitLock::new(ParkInner {
                tree: WaitTree::new(),
                fairness,
            })
        }
    }
}

impl<F: ParkFairness> ParkingLot<F> {
    pub async unsafe fn park<P: Parker>(
        &self,
        address: usize,
        validate: impl FnOnce() -> Option<ParkToken>,
        before_park: impl FnOnce(),
        timed_out: impl FnOnce(Parked),
    ) {
    }

    pub unsafe fn unpark<P: Parker>(
        &self,
        address: usize,
        mut filter: impl FnMut(Parked) -> Unparked,
        before_unpark: impl FnOnce(),
    ) {
        let mut unparked = self.inner.with::<P, _>(|inner| {
            let queue = WaitQueue::from_addr(&inner.tree, address);
            let mut nodes = queue.iter();
            let mut unparked = None;

            while let Some(node) = nodes.next() {
                let park_node = crate::container_of!(node.as_ptr(), ParkNode, wait_node);
                let parked = inner.parked(
                    (*park_node).token.get(),
                    queue.is_empty(),
                );

                match filter(parked) {
                    Unparked::Stop => break,
                    Unparked::Skip => continue,
                    Unparked::Unpark(new_token) => {
                        (*park_node).token.set(new_token);
                        (*park_node).unpark_next.set(unparked);
                        unparked = Some(NonNull::from(&*park_node));
                        queue.remove(Pin::new_unchecked(node.as_ref()));
                    }
                }
            }

            before_unpark();
            unparked
        });

        while let Some(park_node) = unparked {
            unparked = park_node.as_ref().unpark_next.get();
            park_node.atomic_waker.wake();
        }
    }
}
