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
use core::{
    cell::{Cell, UnsafeCell},
    hint::unreachable_unchecked,
    marker::PhantomPinned,
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{fence, AtomicUsize, Ordering},
};

const UNLOCKED: usize = 0;
const LOCKED: usize = 1;
const WAKING: usize = 2;
const WAITING: usize = !3;

#[repr(align(4))]
struct Waiter {
    _pinned: PhantomPinned,
    prev: Cell<Option<NonNull<Self>>>,
    next: Cell<Option<NonNull<Self>>>,
    tail: Cell<Option<NonNull<Self>>>,
    unpark: fn(Pin<&Waiter>),
}

pub struct Lock<T> {
    state: AtomicUsize,
    value: UnsafeCell<T>,
}

impl<T> Lock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED),
            value: UnsafeCell::new(value),
        }
    }

    pub fn with<P: Parker, F>(&self, f: impl FnOnce(&mut T) -> F) -> F {
        self.acquire::<P>();
        let result = f(unsafe { &mut *self.value.get() });
        self.release();
        result
    }

    #[inline]
    fn acquire<P: Parker>(&self) {
        match self.state.compare_exchange_weak(
            UNLOCKED,
            LOCKED,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(_) => self.acquire_slow::<P>(),
        }
    }

    #[cold]
    fn acquire_slow<P: Parker>(&self) {
        struct ParkWaiter<P> {
            parker: P,
            waiter: Waiter,
        }

        let park_waiter = ParkWaiter {
            parker: P::default(),
            waiter: Waiter {
                _pinned: PhantomPinned,
                prev: Cell::new(None),
                next: Cell::new(None),
                tail: Cell::new(None),
                unpark: |waiter| unsafe {
                    let park_waiter = crate::container_of!(&*waiter, ParkWaiter<P>, waiter);
                    let parker = Pin::new_unchecked(&(*park_waiter).parker);
                    parker.unpark();
                },
            },
        };

        let park_waiter = unsafe { Pin::new_unchecked(&park_waiter) };
        let parker = unsafe { park_waiter.map_unchecked(|pw| &pw.parker) };
        let waiter = &park_waiter.waiter;

        let mut adaptive_spin = 0;
        let mut did_prepare = false;
        let mut state = self.state.load(Ordering::Relaxed);

        loop {
            if state & LOCKED == 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state | LOCKED,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(e) => state = e,
                }
                continue;
            }

            let head = NonNull::new((state & WAITING) as *mut Waiter);
            if head.is_none() && P::yield_now(adaptive_spin) {
                adaptive_spin = adaptive_spin.wrapping_add(1);
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            waiter.prev.set(None);
            waiter.next.set(head);
            waiter.tail.set(match head {
                Some(_) => None,
                None => Some(NonNull::from(waiter)),
            });

            if !did_prepare {
                parker.prepare();
                did_prepare = true;
            }

            if let Err(e) = self.state.compare_exchange_weak(
                state,
                (state & !WAITING) | (waiter as *const _ as usize),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                state = e;
                continue;
            }

            let timed_out = parker.park(None);
            debug_assert!(!timed_out);

            adaptive_spin = 0;
            did_prepare = false;
            state = self.state.load(Ordering::Relaxed);
        }
    }

    #[inline]
    fn release(&self) {
        let state = self.state.fetch_sub(LOCKED, Ordering::Release);
        if (state & WAITING != 0) && (state & WAKING == 0) {
            self.release_slow();
        }
    }

    #[cold]
    fn release_slow(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if (state & WAITING == 0) || (state & (LOCKED | WAKING) != 0) {
                return;
            }
            match self.state.compare_exchange_weak(
                state,
                state | WAKING,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => break state |= WAKING,
                Err(e) => state = e,
            }
        }

        'dequeue: loop {
            let (head, tail) = unsafe {
                let head = NonNull::new_unchecked((state & WAITING) as *mut Waiter);
                let tail = head.as_ref().tail.get().unwrap_or_else(|| {
                    let mut current = head;
                    loop {
                        let next = current.as_ref().next.get();
                        let next = next.unwrap_or_else(|| unreachable_unchecked());
                        next.as_ref().prev.set(Some(current));
                        match next.as_ref().tail.get() {
                            None => current = next,
                            Some(tail) => {
                                head.as_ref().tail.set(Some(tail));
                                return tail;
                            }
                        }
                    }
                });
                (&*head.as_ptr(), &*tail.as_ptr())
            };

            if state & LOCKED != 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state & !WAKING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return,
                    Err(e) => state = e,
                }
                continue;
            }

            match tail.prev.get() {
                Some(new_tail) => {
                    head.tail.set(Some(new_tail));
                    self.state.fetch_and(!WAKING, Ordering::Release);
                }
                None => loop {
                    match self.state.compare_exchange_weak(
                        state,
                        state & LOCKED,
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => break,
                        Err(e) => state = e,
                    }

                    if (state & WAITING) != 0 {
                        fence(Ordering::Acquire);
                        continue 'dequeue;
                    }
                },
            }

            (tail.unpark)(unsafe { Pin::new_unchecked(tail) });
            return;
        }
    }
}
