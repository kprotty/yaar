use super::AutoResetEvent;
use core::{
    mem::MaybeUninit,
    ptr::NonNull,
    marker::PhantomData,
    cell::{Cell, UnsafeCell},
    hint::unreachable_unchecked,
    sync::atomic::{fence, Ordering, AtomicUsize},
};

#[repr(align(4))]
pub(crate) struct Node<Event> {
    pub event: Cell<MaybeUninit<Event>>,
    pub prev: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    pub next: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    pub tail: Cell<MaybeUninit<Option<NonNull<Self>>>>,
}

impl<Event> Node<Event> {
    pub fn new() -> Self {
        Self {
            event: Cell::new(MaybeUninit::uninit()),
            prev: Cell::new(MaybeUninit::uninit()),
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
        }
    }

    pub unsafe fn event(&self) -> &Event {
        let maybe_event = &*self.event.as_ptr();
        &*maybe_event.as_ptr()
    }
}

const MUTEX_LOCK: usize = 1 << 0;
const QUEUE_LOCK: usize = 1 << 1;
const QUEUE_MASK: usize = !(MUTEX_LOCK | QUEUE_LOCK);

pub(crate) struct Lock<Event, T> {
    state: AtomicUsize,
    value: UnsafeCell<T>,
    _phantom: PhantomData<E>,
}

impl<Event: AutoResetEvent, T> Lock<Event, T> {
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicUsize::new(0),
            value: UnsafeCell::new(value),
            _phantom: PhantomData,
        }
    }

    pub fn locked<R>(
        &self,
        node: &Node<Event>,
        init_event: impl FnOnce() -> Event,
        critical_section: impl FnOnce(&mut T) -> R,
    ) -> R {
        self.lock(init_event);
        let result = f(unsafe { &mut *self.value.get() });
        self.unlock();
        result
    }

    #[inline]
    fn lock<F: impl FnOnce() -> Event>(&self, init_event: Option<F>) {
        if self
            .state
            .compare_exchange_weak(0, MUTEX_LOCK, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.lock_slow(node, init_event);
        }
    }

    #[cold]
    fn lock_slow<F: impl FnOnce() -> Event>(
        &self,
        node: &Node<Event>,
        mut init_event: Option<F>,
    ) {
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

            if let Some(init_event) = init_event.take() {
                node.event.set(MaybeUninit::new(init_event()));
            }

            node.prev.set(MaybeUninit::new(None));
            node.next.set(MaybeUninit::new(head));
            node.tail.set(MaybeUninit::new(
                if head.is_none() {
                    NonNull::new(node as *const _ as *mut _)
                } else {
                    None
                }
            ));

            if let Err(e) = self.state.compare_exchange_weak(
                state,
                (node as *const _ as usize) | (state & !QUEUE_MASK),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                state = e;
                continue;
            }

            node.event().wait();
            spin = 0;
            node.prev.set(MaybeUninit::new(None));
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
                        match current.tail.get().assume_init() {
                            Some(tail) => {
                                head.tail.set(MaybeUninit::new(Some(tail)));
                                break &*tail.as_ptr();
                            },
                            None => {
                                let next = current.next.get().assume_init();
                                let next = next.unwrap_or_else(|| unreachable_unchecked());
                                let next = &*next.as_ptr();
                                let current_ptr = NonNull::new(current as *const _ as *mut _);
                                next.prev.set(MaybeUninit::new(current_ptr));
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

                match tail.prev.get().assume_init() {
                    Some(new_tail) => {
                        head.tail.set(MaybeUninit::new(Some(new_tail)));
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
                            continue;
                        }
                    },
                }

                tail.event().set();
                return;
            }
        }
    }
}