use super::AutoResetEvent;
use core::{
    ptr::NonNull,
    marker::PhantomData,
    cell::{Cell, UnsafeCell},
    hint::unreachable_unchecked,
    sync::atomic::{fence, Ordering, AtomicUsize},
};

#[repr(align(4))]
struct Node<Event> {
    event: NonNull<Event>,
    prev: Cell<Option<NonNull<Self>>>,
    next: Cell<Option<NonNull<Self>>>,
    tail: Cell<Option<NonNull<Self>>>,
}

const MUTEX_LOCK: usize = 1 << 0;
const QUEUE_LOCK: usize = 1 << 1;
const QUEUE_MASK: usize = !(MUTEX_LOCK | QUEUE_LOCK);

pub struct Lock<Event, T> {
    state: AtomicUsize,
    value: UnsafeCell<T>,
    _phantom: PhantomData<Event>,
}

impl<Event, T> Lock<Event, T> {
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicUsize::new(0),
            value: UnsafeCell::new(value),
            _phantom: PhantomData,
        }
    }
}

impl<Event: AutoResetEvent, T> Lock<Event, T> {
    pub fn locked<R>(
        &self,
        event: &Event,
        critical_section: impl FnOnce(&mut T) -> R,
    ) -> R {
        self.lock(event);
        let result = critical_section(unsafe {
            &mut *self.value.get()
        });
        self.unlock();
        result
    }

    #[inline]
    fn lock(&self, event: &Event) {
        if self
            .state
            .compare_exchange_weak(0, MUTEX_LOCK, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.lock_slow(event);
        }
    }

    #[cold]
    fn lock_slow(&self, event: &Event) {
        let node = Node {
            event: NonNull::new(event as *const _ as *mut _).unwrap(),
            prev: Cell::new(None),
            next: Cell::new(None),
            tail: Cell::new(None),
        };

        let mut spin: usize = 0;
        let mut state = self.state.load(Ordering::Relaxed);

        loop {
            if state & MUTEX_LOCK == 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state | MUTEX_LOCK,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(e) => state = e,
                }
                continue;
            }

            let head = NonNull::new((state & QUEUE_MASK) as *mut Node<Event>);
            if head.is_none() && !Event::yield_now(spin) {
                spin = spin.wrapping_add(1);
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            node.prev.set(None);
            node.next.set(head);
            node.tail.set(match head {
                Some(_) => None,
                None => NonNull::new(&node as *const _ as *mut _),
            });

            if let Err(e) = self.state.compare_exchange_weak(
                state,
                (&node as *const _ as usize) | (state & !QUEUE_MASK),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                state = e;
                continue;
            }

            event.wait();
            spin = 0;
            state = self.state.load(Ordering::Relaxed);
        }
    }

    #[inline]
    fn unlock(&self) {
        let state = self.state.fetch_and(!MUTEX_LOCK, Ordering::Release);
        if (state & QUEUE_MASK != 0) && (state & QUEUE_LOCK == 0) {
            self.unlock_slow();
        }
    }

    #[cold]
    fn unlock_slow(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if (state & QUEUE_MASK == 0) || (state & QUEUE_LOCK != 0) {
                return;
            }
            match self.state.compare_exchange_weak(
                state,
                state | QUEUE_LOCK,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(e) => state = e,
            }
        }

        'outer: loop {
            unsafe {
                let head = &*((state & QUEUE_MASK) as *const Node<Event>);
                let tail = {
                    let mut current = head;
                    loop {
                        match current.tail.get() {
                            Some(tail) => {
                                head.tail.set(Some(tail));
                                break &*tail.as_ptr();
                            },
                            None => {
                                let next = current.next.get();
                                let next = next.unwrap_or_else(|| unreachable_unchecked());
                                let next = &*next.as_ptr();
                                next.prev.set(NonNull::new(current as *const _ as *mut _));
                                current = next;
                            },
                        }
                    }
                };

                if state & MUTEX_LOCK != 0 {
                    match self.state.compare_exchange_weak(
                        state,
                        state & !QUEUE_LOCK,
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => return,
                        Err(e) => state = e,
                    }
                    fence(Ordering::Acquire);
                    continue;
                }

                match tail.prev.get() {
                    Some(new_tail) => {
                        head.tail.set(Some(new_tail));
                        self.state.fetch_and(!QUEUE_LOCK, Ordering::Release);
                    },
                    None => loop {
                        match self.state.compare_exchange_weak(
                            state,
                            state & MUTEX_LOCK,
                            Ordering::Release,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => break,
                            Err(e) => state = e,
                        }
                        if state & QUEUE_MASK != 0 {
                            fence(Ordering::Acquire);
                            continue 'outer;
                        }
                    },
                }

                (&*tail.event.as_ptr()).set();
                return;
            }
        }
    }
}