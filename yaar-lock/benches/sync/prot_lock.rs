#![allow(unused)]

use std::{
    fmt,
    mem::replace,
    ptr::NonNull,
    cell::{Cell, UnsafeCell},
    ops::{Deref, DerefMut},
    sync::{
        Condvar,
        Mutex as StdMutex,
        atomic::{fence, spin_loop_hint, Ordering, AtomicUsize},
    },
};

const EMPTY: usize = 0;
const WAITING: usize = 1;
const NOTIFIED: usize = 2;

#[repr(align(4))]
struct Node {
    state: AtomicUsize,
    mutex: StdMutex<usize>,
    condvar: Condvar,
    prev: Cell<Option<NonNull<Self>>>,
    next: Cell<Option<NonNull<Self>>>,
    tail: Cell<Option<NonNull<Self>>>,
}

impl Node {
    fn with_local<T>(f: impl FnOnce(&Self) -> T) -> T {
        thread_local!(static NODE: Node = Node {
            state: AtomicUsize::new(EMPTY),
            mutex: StdMutex::new(EMPTY),
            condvar: Condvar::new(),
            prev: Cell::new(None),
            next: Cell::new(None),
            tail: Cell::new(None),
        });
        NODE.with(f)
    }

    fn notify(&self) {
        let mut state = EMPTY;
        loop {
            if state == NOTIFIED {
                return;
            }
            match self.state.compare_exchange_weak(
                state,
                if state == EMPTY { NOTIFIED } else { EMPTY },
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Err(e) => state = e,
                Ok(WAITING) => break,
                Ok(_) => return,
            }
        }

        let mut state = self.mutex.lock().unwrap();
        if *state == EMPTY {
            *state = NOTIFIED;
            return;
        }

        *state = EMPTY;
        self.condvar.notify_one();
    }

    fn wait(&self) {
        let mut state = NOTIFIED;
        loop {
            match self.state.compare_exchange_weak(
                state,
                if state == EMPTY { WAITING } else { EMPTY },
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Err(e) => state = e,
                Ok(NOTIFIED) => return,
                Ok(_) => break,
            }
        }

        let mut state = self.mutex.lock().unwrap();
        if *state == NOTIFIED {
            *state = EMPTY;
            return;
        }

        *state = WAITING;
        while *state == WAITING {
            state = self.condvar.wait(state).unwrap();
        }
    }
}

const UNLOCKED: usize = 0 << 0;
const LOCKED:   usize = 1 << 0;
const WAKING:   usize = 1 << 1;
const MASK:     usize = !(UNLOCKED | LOCKED | WAKING);

pub struct Mutex<T> {
    state: AtomicUsize,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    #[inline]
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED),
            value: UnsafeCell::new(value),
        }
    }

    #[inline]
    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value.get() }
    }

    #[inline]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        let state = self.state.fetch_or(LOCKED, Ordering::Acquire);
        if state & LOCKED == 0 {
            Some(MutexGuard{ mutex: self })
        } else {
            None
        }
    }

    #[inline]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        let state = self.state.fetch_or(LOCKED, Ordering::Acquire);
        if state & LOCKED != 0 {
            self.lock_slow(state);
        }
        MutexGuard{ mutex: self }
    }

    #[cold]
    fn lock_slow(&self, mut state: usize) {
        Node::with_local(|node| {
            let mut waking = false;
            let mut spin: usize = 0;
            
            loop {
                if state & LOCKED == 0 {
                    state = self.state.fetch_or(LOCKED, Ordering::Acquire);
                    if state & LOCKED == 0 {
                        return;
                    }
                    continue;
                }

                let head = NonNull::new((state & MASK) as *mut Node);
                if head.is_none() && spin < 100 {
                    spin_loop_hint();
                    spin = spin.wrapping_add(1);
                    state = self.state.load(Ordering::Relaxed);
                    continue;
                }

                node.prev.set(None);
                node.next.set(head);
                node.tail.set(match head {
                    Some(_) => None,
                    None => NonNull::new(node as *const _ as *mut _),
                });

                if let Err(e) = self.state.compare_exchange_weak(
                    state,
                    (node as *const _ as usize) | (state & !MASK),
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    state = e;
                    continue;
                }

                node.wait();
                spin = 0;
                state = self.state.fetch_and(!WAKING, Ordering::Relaxed) & !WAKING;
            }
        });
    }

    #[inline]
    pub unsafe fn force_unlock(&self) {
        let state = self.state.fetch_and(!LOCKED, Ordering::Release);
        if (state & WAKING == 0) && (state & MASK != 0) {
            self.unlock_slow(state & !LOCKED);
        }
    }

    #[cold]
    unsafe fn unlock_slow(&self, mut state: usize) {
        loop {
            if (state & MASK == 0) || (state & WAKING != 0) || (state & LOCKED != 0) {
                return;
            }
            match self.state.compare_exchange_weak(
                state,
                state | WAKING,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Err(e) => state = e,
                Ok(_) => {
                    state |= WAKING;
                    break;
                },
            }
        }

        'outer: loop {
            let head = &*((state & MASK) as *const Node);
            let tail = {
                let mut current = head;
                loop {
                    match current.tail.get() {
                        Some(tail) => {
                            head.tail.set(Some(tail));
                            break &*tail.as_ptr()
                        },
                        None => {
                            let next = &*current.next.get().unwrap().as_ptr();
                            let ptr = current as *const _ as *mut _; 
                            next.prev.set(NonNull::new(ptr));
                            current = next;
                        },
                    }
                }
            };

            if state & LOCKED != 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state & !WAKING,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(e) => state = e,
                }
                continue;
            }

            if let Some(new_tail) = tail.prev.get() {
                head.tail.set(Some(new_tail));
                fence(Ordering::Release);
            } else {
                loop {
                    match self.state.compare_exchange_weak(
                        state,
                        state & !MASK,
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => { state &= !MASK; break;},
                        Err(e) => state = e,
                    }
                    if state & MASK != 0 {
                        fence(Ordering::Acquire);
                        continue 'outer;
                    }
                }
            }

            tail.notify();
            return;
        }
    }
}

pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
}

impl<'a, T> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        unsafe { self.mutex.force_unlock() };
    }
}

impl<'a, T> DerefMut for MutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.value.get() }
    }
}

impl<'a, T> Deref for MutexGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.mutex.value.get() }
    }
}

impl<'a, T: fmt::Debug> fmt::Debug for MutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<'a, T: fmt::Display> fmt::Display for MutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}