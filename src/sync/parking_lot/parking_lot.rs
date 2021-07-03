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

use super::{
    atomic_waker::AtomicWaker,
    lock::Lock as WaitLock,
    queue::{WaitNode, WaitQueue, WaitTree},
};
use crate::sync::{
    atomic::{AtomicBool, Ordering},
    parker::Parker,
};
use core::{cell::Cell, marker::PhantomData, mem::drop, pin::Pin, ptr::NonNull};

#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub struct ParkToken(pub usize);

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Unparked {
    Stop,
    Skip,
    Unpark(ParkToken),
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct Parked {
    sealed: (),
    pub token: ParkToken,
    pub is_last: bool,
    pub random: usize,
}

#[derive(Default)]
struct ParkNode {
    wait_node: WaitNode,
    token: Cell<ParkToken>,
    atomic_waker: AtomicWaker,
    is_waiting: AtomicBool,
    unpark_next: Cell<Option<NonNull<ParkNode>>>,
}

pub struct ParkingLot {
    tree: WaitLock<WaitTree>,
}

impl ParkingLot {
    pub const fn new() -> Self {
        Self {
            tree: WaitLock::new(WaitTree::new()),
        }
    }
}

impl ParkingLot {
    pub async unsafe fn park<P, Validate, BeforePark, TimedOut>(
        &self,
        address: usize,
        validate: Validate,
        before_park: BeforePark,
        timed_out: TimedOut,
    ) -> Result<ParkToken, ()>
    where
        P: Parker,
        Validate: FnOnce() -> Option<ParkToken>,
        BeforePark: FnOnce() -> (),
        TimedOut: FnOnce(Parked) -> (),
    {
        let park_node = ParkNode::default();
        let park_node = Pin::new_unchecked(&park_node);

        if !self.tree.with::<P, _, _>(|tree| {
            let wait_queue = WaitQueue::from_addr(tree, address);
            let token = match validate() {
                Some(token) => token,
                None => return false,
            };

            wait_queue.insert(park_node.map_unchecked(|pn| &pn.wait_node));
            park_node.is_waiting.store(true, Ordering::Relaxed);
            park_node.token.set(token);
            true
        }) {
            return Err(());
        }

        struct ParkWaiter<'pl, 'pn, P: Parker, T: FnOnce(Parked)> {
            park_node: Pin<&'pn ParkNode>,
            parking_lot: &'pl ParkingLot,
            timed_out: Option<T>,
            parker: PhantomData<P>,
        }

        impl<'pl, 'pn, P: Parker, T: FnOnce(Parked)> Drop for ParkWaiter<'pl, 'pn, P, T> {
            #[inline]
            fn drop(&mut self) {
                if self.park_node.is_waiting.load(Ordering::Relaxed) {
                    self.drop_slow();
                }
            }
        }

        impl<'pl, 'pn, P: Parker, T: FnOnce(Parked)> ParkWaiter<'pl, 'pn, P, T> {
            #[cold]
            fn drop_slow(&mut self) {
                self.parking_lot.tree.with::<P, _, _>(|tree| unsafe {
                    if !self.park_node.is_waiting.load(Ordering::Relaxed) {
                        return;
                    }

                    let wait_node = self.park_node.map_unchecked(|pn| &pn.wait_node);
                    let wait_queue = WaitQueue::from_node(tree, wait_node);
                    wait_queue.remove(wait_node);

                    let timed_out = self.timed_out.take().expect("no timeout callback");
                    timed_out(Parked {
                        sealed: (),
                        token: self.park_node.token.get(),
                        is_last: wait_queue.is_empty(),
                        random: tree.gen_random(self as *const _ as usize),
                    })
                })
            }
        }

        let _waiter = ParkWaiter {
            park_node,
            parking_lot: self,
            timed_out: Some(timed_out),
            parker: PhantomData::<P>,
        };

        before_park();
        park_node.atomic_waker.wait().await;
        Ok(park_node.token.get())
    }

    pub unsafe fn unpark_one<P, U>(&self, address: usize, on_unpark: U)
    where
        P: Parker,
        U: FnOnce(Option<Parked>) -> ParkToken,
    {
        let on_unpark = Cell::new(Some(on_unpark));
        let filter = |parked| match on_unpark.replace(None) {
            Some(on_unpark) => Unparked::Unpark(on_unpark(Some(parked))),
            None => Unparked::Stop,
        };
        let before_unpark = || match on_unpark.replace(None) {
            Some(on_unpark) => drop(on_unpark(None)),
            None => {}
        };

        self.unpark::<P, _, _>(address, filter, before_unpark)
    }

    pub unsafe fn unpark_all<P>(&self, address: usize, token: ParkToken)
    where
        P: Parker,
    {
        let filter = |_| Unparked::Unpark(token);
        let before_unpark = || {};
        self.unpark::<P, _, _>(address, filter, before_unpark);
    }

    pub unsafe fn unpark<P, Filter, BeforeUnpark>(
        &self,
        address: usize,
        mut filter: Filter,
        before_unpark: BeforeUnpark,
    ) where
        P: Parker,
        Filter: FnMut(Parked) -> Unparked,
        BeforeUnpark: FnOnce(),
    {
        let mut unparked = self.tree.with::<P, _, _>(|tree| {
            let wait_queue = WaitQueue::from_addr(tree, address);
            let mut nodes = wait_queue.iter().peekable();
            let mut unparked = None;

            while let Some(node) = nodes.next() {
                let park_node = crate::container_of!(node.as_ptr(), ParkNode, wait_node);
                let park_node = Pin::new_unchecked(&*park_node);

                let parked = Parked {
                    sealed: (),
                    token: park_node.token.get(),
                    is_last: nodes.peek().is_none(),
                    random: tree.gen_random(node.as_ptr() as usize),
                };

                match filter(parked) {
                    Unparked::Stop => break,
                    Unparked::Skip => continue,
                    Unparked::Unpark(new_token) => {
                        wait_queue.remove(park_node.map_unchecked(|pn| &pn.wait_node));
                        park_node.is_waiting.store(false, Ordering::Relaxed);

                        park_node.token.set(new_token);
                        park_node.unpark_next.set(unparked);
                        unparked = Some(NonNull::from(&*park_node));
                    }
                }
            }

            before_unpark();
            unparked
        });

        while let Some(park_node) = unparked {
            unparked = park_node.as_ref().unpark_next.get();
            park_node.as_ref().atomic_waker.wake();
        }
    }
}
