use super::Lock;
use crate::event::AutoResetEvent;
use core::{
    cell::Cell,
    mem::MaybeUninit,
    ptr::{write, drop_in_place, NonNull},
};

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum ParkResult {
    Invalid,
    Cancelled,
    Unparked(usize),
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum UnparkFilter {
    Stop,
    Skip,
    Unpark(usize),
}

#[derive(Copy, Clone, Debug)]
pub struct UnparkResult {
    pub unparked: usize,
    pub skipped: usize,
    pub has_more: bool,
    pub(crate) _sealed: (),
}

pub struct Parker<E> {
    queue_lock: Lock<E>,
    queue: Cell<Option<NonNull<Waiter<E>>>>,
}

impl<E> Default for Parker<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E> Parker<E> {
    pub const fn new() -> Self {
        Self {
            queue_lock: Lock::new(),
            queue: Cell::new(None),
        }
    }
}

impl<E: AutoResetEvent> Parker<E> {
    #[inline]
    unsafe fn with_queue<T>(
        &self,
        f: impl FnOnce(&mut Option<NonNull<Waiter<E>>>) -> T,
    ) -> T {
        use lock_api::RawMutex;
        self.queue_lock.lock();
        let result = f(&mut *self.queue.as_ptr());
        self.queue_lock.unlock();
        result
    }

    pub unsafe fn park(
        &self,
        token: usize,
        validate: impl FnOnce() -> bool,
        try_park: impl FnOnce(&E) -> bool,
        cancel: impl FnOnce(bool),
    ) -> ParkResult {
        let waiter = MaybeUninit::<Waiter<E>>::uninit();
        let waiter_ptr = NonNull::new_unchecked(waiter.as_ptr() as *mut _);
        let waiter_ref = &*waiter_ptr.as_ptr();

        if !self.with_queue(|head| validate() && {
            write(waiter_ptr.as_ptr(), Waiter {
                event: E::default(),
                state: Cell::new(WaitState::Parked(token)),
                prev: Cell::new(None),
                next: Cell::new(*head),
                tail: Cell::new({
                    if let Some(head) = *head {
                        let head = &*head.as_ptr();
                        head.prev.set(Some(waiter_ptr));
                        head.tail.get()
                    } else {
                        *head = Some(waiter_ptr);
                        waiter_ptr
                    }
                }),
            });
            true
        }) {
            return ParkResult::Invalid;
        }

        // Acquire
        if try_park(&waiter_ref.event) {
            drop_in_place(waiter_ptr.as_ptr());
            return match waiter_ref.state.get() {
                WaitState::Unparked(token) => ParkResult::Unparked(token),
                WaitState::Parked(_) => unreachable!("AutoResetEvent::set() woke without unpark()"),
            };
        }

        let result = self.with_queue(|queue| match waiter_ref.state.get() {
            WaitState::Unparked(token) => ParkResult::Unparked(token),
            WaitState::Parked(_) => {
                let was_last_waiter = Self::remove(queue, waiter_ref);
                cancel(was_last_waiter);
                ParkResult::Cancelled
            },
        });

        drop_in_place(waiter_ptr.as_ptr());
        result
    }

    #[inline]
    pub unsafe fn unpark_all(
        &self,
        mut unpark: impl FnMut(UnparkResult, usize) -> usize,
        callback: impl FnOnce(UnparkResult),
    ) {
        self.unpark(
            |result, token| UnparkFilter::Unpark(unpark(result, token)),
            callback,
        )
    }

    #[inline]
    pub unsafe fn unpark_one(
        &self,
        unpark: impl FnOnce(UnparkResult, usize) -> usize,
        callback: impl FnOnce(UnparkResult),
    ) {
        let mut unpark = Some(unpark);
        self.unpark(
            |result, token| match unpark.take() {
                Some(unpark) => UnparkFilter::Unpark(unpark(result, token)),
                None => UnparkFilter::Stop,
            },
            callback,
        )
    }

    pub unsafe fn unpark(
        &self,
        mut filter: impl FnMut(UnparkResult, usize) -> UnparkFilter,
        callback: impl FnOnce(UnparkResult),
    ) {
        // List of waiters that need to be unparked
        let mut unparked_list = WaiterList::new();

        self.with_queue(|head| {
            let mut result = UnparkResult {
                unparked: 0,
                skipped: 0,
                has_more: false,
                _sealed: (),
            };

            // Start from the tail of the queue
            let mut current = *head;
            current = current.map(|p| (&*p.as_ptr()).tail.get());
            while let Some(waiter) = current {
                let waiter = &*waiter.as_ptr();
                current = waiter.prev.get();
                result.has_more = current.is_some();

                let token = match waiter.state.get() {
                    WaitState::Parked(token) => token,
                    WaitState::Unparked(_) => unreachable!("Thread unparked still in park list"),
                };

                match filter(result, token) {
                    UnparkFilter::Stop => break,
                    UnparkFilter::Skip => result.skipped += 1,
                    UnparkFilter::Unpark(token) => {
                        result.unparked += 1;
                        Self::remove(head, waiter);
                        unparked_list.push(waiter);
                        waiter.state.set(WaitState::Unparked(token));
                    },
                }
            }

            result.has_more = result.has_more || result.skipped > 0;
            callback(result);
        });

        for waiter in unparked_list.iter() {
            waiter.event.set();
        }
    }

    unsafe fn remove(
        head: &mut Option<NonNull<Waiter<E>>>,
        waiter: &Waiter<E>,
    ) -> bool {
        // Get the head of the queue, returning false if the queue is empty.
        let head_ref = match *head {
            Some(p) => &*p.as_ptr(),
            None => return false,
        };

        // Fix up the links of surrounding waiter nodes
        let prev = waiter.prev.get();
        let next = waiter.next.get();
        if let Some(prev) = prev {
            (&*prev.as_ptr()).next.set(next);
        }
        if let Some(next) = next {
            (&*next.as_ptr()).prev.set(prev);
        }

        let tail_ptr = head_ref.tail.get();
        let waiter_ptr = NonNull::new_unchecked(waiter as *const _ as *mut _);

        // Fix the tail of the queue & the head if this was the last waiter.
        let mut was_last_waiter = false;
        if waiter_ptr == tail_ptr {
            if let Some(prev) = prev {
                head_ref.tail.set(prev);
            } else {
                *head = None;
                was_last_waiter = true;
            }
        }

        was_last_waiter
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum WaitState {
    Parked(usize),
    Unparked(usize),
}

struct Waiter<E> {
    event: E,
    state: Cell<WaitState>,
    prev: Cell<Option<NonNull<Self>>>,
    next: Cell<Option<NonNull<Self>>>,
    tail: Cell<NonNull<Self>>,
}

struct WaiterList<E> {
    array_size: usize,
    array: [MaybeUninit<NonNull<Waiter<E>>>; 16],
    overflow_head: Option<NonNull<Waiter<E>>>,
    overflow_tail: Option<NonNull<Waiter<E>>>,
}

impl<E> WaiterList<E> {
    fn new() -> Self {
        Self {
            array_size: 0,
            array: unsafe { MaybeUninit::uninit().assume_init() },
            overflow_head: None,
            overflow_tail: None,
        }
    }

    unsafe fn push(&mut self, waiter: &Waiter<E>) {
        let waiter_ptr = NonNull::new_unchecked(waiter as *const _ as *mut _);

        if self.array_size < self.array.len() {
            let pos = self.array.get_unchecked_mut(self.array_size);
            *pos = MaybeUninit::new(waiter_ptr);
            self.array_size += 1;
            return;
        }

        if let Some(tail) = self.overflow_tail {
            (&*tail.as_ptr()).next.set(Some(waiter_ptr));
        } else {
            self.overflow_head = Some(waiter_ptr);
        }

        waiter.next.set(None);
        self.overflow_tail = Some(waiter_ptr);
    }

    unsafe fn iter(&self) -> impl Iterator<Item = &'_ Waiter<E>> + '_ {
        struct Iter<'a, E> {
            list: &'a WaiterList<E>,
            array_index: usize,
            overflow_node: Option<NonNull<Waiter<E>>>,
        }

        impl<'a, E> Iterator for Iter<'a, E> {
            type Item = &'a Waiter<E>;

            fn next(&mut self) -> Option<Self::Item> {
                unsafe {
                    if self.array_index < self.list.array_size {
                        let waiter = self.list.array.get_unchecked(self.array_index);
                        self.array_index += 1;
                        return Some(&*(*waiter).assume_init().as_ptr());
                    }

                    if let Some(waiter) = self.overflow_node {
                        let waiter = &*waiter.as_ptr();
                        self.overflow_node = waiter.next.get();
                        return Some(waiter);
                    }

                    None
                }
            }
        }

        Iter {
            list: self,
            array_index: 0,
            overflow_node: self.overflow_head,
        }
    }
}