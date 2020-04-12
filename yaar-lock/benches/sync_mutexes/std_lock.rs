use std::{
    cell::{Cell, UnsafeCell},
    fmt,
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
    ptr::NonNull,
    sync::{
        atomic::{fence, AtomicUsize, Ordering},
        Condvar, Mutex as StdMutex,
    },
};

#[derive(Copy, Clone, Eq, PartialEq)]
enum NodeState {
    Empty,
    Waiting,
    Notified,
}

#[repr(align(4))]
struct Node {
    initialized: Cell<bool>,
    mutex: Cell<MaybeUninit<StdMutex<NodeState>>>,
    condvar: Cell<MaybeUninit<Condvar>>,
    prev: Cell<Option<NonNull<Self>>>,
    next: Cell<Option<NonNull<Self>>>,
    tail: Cell<Option<NonNull<Self>>>,
}

unsafe impl Sync for Node {}

impl Node {
    pub const fn new() -> Self {
        Self {
            initialized: Cell::new(false),
            mutex: Cell::new(MaybeUninit::uninit()),
            condvar: Cell::new(MaybeUninit::uninit()),
            prev: Cell::new(None),
            next: Cell::new(None),
            tail: Cell::new(None),
        }
    }

    #[cold]
    pub fn lazy_init(&self) {
        self.condvar.set(MaybeUninit::new(Condvar::new()));
        self.mutex
            .set(MaybeUninit::new(StdMutex::new(NodeState::Empty)));
    }

    pub fn with_local<T>(f: impl FnOnce(&Self) -> T) -> T {
        thread_local!(static NODE: Node = Node::new());
        NODE.with(f)
    }

    fn signal(&self) -> (&StdMutex<NodeState>, &Condvar) {
        unsafe {
            let mutex = &*(&*self.mutex.as_ptr()).as_ptr();
            let condvar = &*(&*self.condvar.as_ptr()).as_ptr();
            (mutex, condvar)
        }
    }

    pub fn wait(&self) {
        let (mutex, condvar) = self.signal();
        let mut state = mutex.lock().unwrap();

        if *state != NodeState::Notified {
            *state = NodeState::Waiting;
            while *state != NodeState::Notified {
                state = condvar.wait(state).unwrap();
            }
        }

        *state = NodeState::Empty;
    }

    pub fn notify(&self) {
        let (mutex, condvar) = self.signal();
        let mut state = mutex.lock().unwrap();

        let notify = *state == NodeState::Waiting;
        *state = NodeState::Notified;
        if notify {
            condvar.notify_one();
        }
    }
}

const MUTEX_LOCK: usize = 1 << 0;
const QUEUE_LOCK: usize = 1 << 1;
const QUEUE_MASK: usize = !(MUTEX_LOCK | QUEUE_LOCK);

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
            state: AtomicUsize::new(0),
            value: UnsafeCell::new(value),
        }
    }

    #[allow(unused)]
    #[inline]
    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    #[allow(unused)]
    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value.get() }
    }

    #[allow(unused)]
    #[inline]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state & MUTEX_LOCK != 0 {
                return None;
            }
            match self.state.compare_exchange_weak(
                state,
                state | MUTEX_LOCK,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(MutexGuard { mutex: self }),
                Err(e) => state = e,
            }
        }
    }

    #[inline]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        if self
            .state
            .compare_exchange_weak(0, MUTEX_LOCK, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.lock_slow();
        }
        MutexGuard { mutex: self }
    }

    #[cold]
    fn lock_slow(&self) {
        Node::with_local(|node| {
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

                let head = NonNull::new((state & QUEUE_MASK) as *mut Node);
                if head.is_none() && !Self::yield_now(spin) {
                    spin = spin.wrapping_add(1);
                    state = self.state.load(Ordering::Relaxed);
                    continue;
                }

                if !node.initialized.get() {
                    node.initialized.set(true);
                    node.lazy_init();
                }

                node.prev.set(None);
                node.next.set(head);
                node.tail.set(match head {
                    Some(_) => None,
                    None => Some(NonNull::from(node)),
                });

                if let Err(e) = self.state.compare_exchange_weak(
                    state,
                    (node as *const _ as usize) | (state & !QUEUE_MASK),
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    state = e;
                    continue;
                }

                node.wait();
                spin = 0;
                state = self.state.load(Ordering::Relaxed);
            }
        });
    }

    #[inline]
    pub unsafe fn force_unlock(&self) {
        if self
            .state
            .compare_exchange(MUTEX_LOCK, 0, Ordering::Release, Ordering::Relaxed)
            .is_err()
        {
            self.unlock_slow();
        }
    }

    #[cold]
    unsafe fn unlock_slow(&self) {
        let mut state = self.state.fetch_and(!MUTEX_LOCK, Ordering::Release) & !MUTEX_LOCK;
        loop {
            if (state & QUEUE_LOCK != 0) || (state & QUEUE_MASK == 0) {
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
            let head = &*((state & QUEUE_MASK) as *const Node);
            let tail = {
                let mut current = head;
                loop {
                    match current.tail.get() {
                        Some(tail) => {
                            head.tail.set(Some(tail));
                            break &*tail.as_ptr();
                        }
                        None => {
                            let next = &*current.next.get().unwrap().as_ptr();
                            next.prev.set(Some(NonNull::from(current)));
                            current = next;
                        }
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
                }
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

            tail.notify();
            return;
        }
    }

    #[cfg(unix)]
    fn yield_now(spin: usize) -> bool {
        if spin < 40 {
            std::thread::yield_now();
            false
        } else {
            true
        }
    }

    #[cfg(all(windows, not(any(target_arch = "x86_64", target_arch = "x86"))))]
    fn yield_now(spin: usize) -> bool {
        if spin < 100 {
            std::sync::atomic::spin_loop_hint();
            false
        } else {
            true
        }
    }

    #[cfg(all(windows, any(target_arch = "x86_64", target_arch = "x86")))]
    fn yield_now(spin: usize) -> bool {
        #[cfg(target_arch = "x86")]
        use std::arch::x86::{CpuidResult, __cpuid};
        #[cfg(target_arch = "x86_64")]
        use std::arch::x86_64::{CpuidResult, __cpuid};

        use std::{
            hint::unreachable_unchecked, slice::from_raw_parts, str::from_utf8_unchecked,
            sync::atomic::spin_loop_hint,
        };

        static IS_AMD: AtomicUsize = AtomicUsize::new(0);
        let is_amd = unsafe {
            match IS_AMD.load(Ordering::Relaxed) {
                0 => {
                    let CpuidResult { ebx, ecx, edx, .. } = __cpuid(0);
                    let vendor = &[ebx, edx, ecx] as *const _ as *const u8;
                    let vendor = from_utf8_unchecked(from_raw_parts(vendor, 3 * 4));
                    let is_amd = vendor == "AuthenticAMD";
                    IS_AMD.store((is_amd as usize) + 1, Ordering::Relaxed);
                    is_amd
                }
                1 => false,
                2 => true,
                _ => unreachable_unchecked(),
            }
        };

        if !is_amd && spin < 1024 {
            (0..(1 << spin).min(100)).for_each(|_| spin_loop_hint());
            false
        } else {
            spin_loop_hint();
            true
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
