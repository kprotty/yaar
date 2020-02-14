use crossbeam_utils::{thread::scope, CachePadded};
use std::{convert::TryInto, iter, sync::Barrier, time};

pub fn main() {
    bench_all("Contended", 1);
    bench_all("Uncontended", 1000);
}

struct Options {
    n_threads: u32,
    n_locks: u32,
    n_ops: u32,
    n_rounds: u32,
}

fn bench_all(name: &str, num_locks: u32) {
    let options = Options {
        n_threads: num_cpus::get().try_into().unwrap(),
        n_locks: num_locks,
        n_ops: 10_000,
        n_rounds: 20,
    };

    println!("------------------------------------------------");
    println!("{} (locks = {})", name, options.n_locks);
    println!("------------------------------------------------");
    bench::<mutexes::Std>(&options);
    bench::<mutexes::ParkingLot>(&options);
    bench::<mutexes::YaarLock>(&options);
    bench::<mutexes::AmdSpin>(&options);
    #[cfg(unix)]
    bench::<mutexes::PosixLock>(&options);
    #[cfg(all(windows, target_env = "msvc"))]
    bench::<mutexes::NtLock>(&options);
    println!();
}

fn bench<M: Mutex>(options: &Options) {
    let mut times = (0..options.n_rounds)
        .map(|_| run_bench::<M>(options))
        .collect::<Vec<_>>();
    times.sort();

    let avg = times.iter().sum::<time::Duration>() / options.n_rounds;
    let min = times[0];
    let max = *times.last().unwrap();

    let avg = format!("{:?}", avg);
    let min = format!("{:?}", min);
    let max = format!("{:?}", max);

    println!(
        "{:<20} avg {:<12} min {:<12} max {:<12}",
        M::LABEL,
        avg,
        min,
        max
    )
}

mod mutexes {
    use super::*;

    pub(crate) type Std = std::sync::Mutex<u32>;
    impl Mutex for Std {
        const LABEL: &'static str = "std::sync::Mutex";
        fn with_lock(&self, f: impl FnOnce(&mut u32)) {
            let mut guard = self.lock().unwrap();
            f(&mut guard)
        }
    }

    pub(crate) type ParkingLot = parking_lot::Mutex<u32>;
    impl Mutex for ParkingLot {
        const LABEL: &'static str = "parking_lot::Mutex";
        fn with_lock(&self, f: impl FnOnce(&mut u32)) {
            let mut guard = self.lock();
            f(&mut guard)
        }
    }

    pub(crate) type AmdSpin = crate::amd_spinlock::AmdSpinlock<u32>;
    impl Mutex for AmdSpin {
        const LABEL: &'static str = "AmdSpinlock";
        fn with_lock(&self, f: impl FnOnce(&mut u32)) {
            let mut guard = self.lock();
            f(&mut guard)
        }
    }

    pub(crate) type YaarLock = yaar_lock::sync::Mutex<u32>;
    impl Mutex for YaarLock {
        const LABEL: &'static str = "yaar_lock";
        fn with_lock(&self, f: impl FnOnce(&mut u32)) {
            let mut guard = self.lock();
            f(&mut guard)
        }
    }

    #[cfg(all(windows, target_env = "msvc"))]
    pub(crate) type NtLock = ntlock::Mutex<u32>;
    #[cfg(all(windows, target_env = "msvc"))]
    impl Mutex for NtLock {
        const LABEL: &'static str = "ntlock";
        fn with_lock(&self, f: impl FnOnce(&mut u32)) {
            let mut guard = self.lock();
            f(&mut guard)
        }
    }

    #[cfg(unix)]
    pub(crate) type PosixLock = posix_lock::Mutex<u32>;
    #[cfg(unix)]
    impl Mutex for PosixLock {
        const LABEL: &'static str = "posix_lock";
        fn with_lock(&self, f: impl FnOnce(&mut u32)) {
            let mut guard = self.lock();
            f(&mut guard)
        }
    }
}

fn random_numbers(seed: u32) -> impl Iterator<Item = u32> {
    let mut random = seed;
    iter::repeat_with(move || {
        random ^= random << 13;
        random ^= random >> 17;
        random ^= random << 5;
        random
    })
}

trait Mutex: Sync + Send + Default {
    const LABEL: &'static str;
    fn with_lock(&self, f: impl FnOnce(&mut u32));
}

fn run_bench<M: Mutex>(options: &Options) -> time::Duration {
    let locks = &(0..options.n_locks)
        .map(|_| CachePadded::new(M::default()))
        .collect::<Vec<_>>();

    let start_barrier = &Barrier::new(options.n_threads as usize + 1);
    let end_barrier = &Barrier::new(options.n_threads as usize + 1);

    let elapsed = scope(|scope| {
        let thread_seeds = random_numbers(0x6F4A955E).scan(0x9BA2BF27, |state, n| {
            *state ^= n;
            Some(*state)
        });
        for thread_seed in thread_seeds.take(options.n_threads as usize) {
            scope.spawn(move |_| {
                start_barrier.wait();
                let indexes = random_numbers(thread_seed)
                    .map(|it| it % options.n_locks)
                    .map(|it| it as usize)
                    .take(options.n_ops as usize);
                for idx in indexes {
                    locks[idx].with_lock(|cnt| *cnt += 1);
                }
                end_barrier.wait();
            });
        }

        std::thread::sleep(time::Duration::from_millis(100));
        start_barrier.wait();
        let start = time::Instant::now();
        end_barrier.wait();
        let elapsed = start.elapsed();

        let mut total = 0;
        for lock in locks.iter() {
            lock.with_lock(|cnt| total += *cnt);
        }
        assert_eq!(total, options.n_threads * options.n_ops);

        elapsed
    })
    .unwrap();
    elapsed
}

mod amd_spinlock {
    use std::{
        cell::UnsafeCell,
        ops,
        sync::atomic::{spin_loop_hint, AtomicBool, Ordering},
    };

    #[derive(Default)]
    pub(crate) struct AmdSpinlock<T> {
        locked: AtomicBool,
        data: UnsafeCell<T>,
    }
    unsafe impl<T: Send> Send for AmdSpinlock<T> {}
    unsafe impl<T: Send> Sync for AmdSpinlock<T> {}

    pub(crate) struct AmdSpinlockGuard<'a, T> {
        lock: &'a AmdSpinlock<T>,
    }

    impl<T> AmdSpinlock<T> {
        pub(crate) fn lock(&self) -> AmdSpinlockGuard<T> {
            loop {
                let was_locked = self.locked.load(Ordering::Relaxed);
                if !was_locked
                    && self
                        .locked
                        .compare_exchange_weak(
                            was_locked,
                            true,
                            Ordering::Acquire,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                {
                    break;
                }
                spin_loop_hint()
            }
            AmdSpinlockGuard { lock: self }
        }
    }

    impl<'a, T> ops::Deref for AmdSpinlockGuard<'a, T> {
        type Target = T;
        fn deref(&self) -> &Self::Target {
            let ptr = self.lock.data.get();
            unsafe { &*ptr }
        }
    }

    impl<'a, T> ops::DerefMut for AmdSpinlockGuard<'a, T> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            let ptr = self.lock.data.get();
            unsafe { &mut *ptr }
        }
    }

    impl<'a, T> Drop for AmdSpinlockGuard<'a, T> {
        fn drop(&mut self) {
            self.lock.locked.store(false, Ordering::Release)
        }
    }
}

#[cfg(unix)]
mod posix_lock {
    use std::{
        cell::UnsafeCell,
        ops::{Deref, DerefMut},
        sync::atomic::{spin_loop_hint, AtomicBool, Ordering},
    };

    pub struct Mutex<T> {
        locked: AtomicBool,
        value: UnsafeCell<T>,
    }

    unsafe impl<T: Send> Send for Mutex<T> {}
    unsafe impl<T: Send> Sync for Mutex<T> {}

    impl<T: Default> Default for Mutex<T> {
        fn default() -> Self {
            Self::new(T::default())
        }
    }

    impl<T> Mutex<T> {
        pub const fn new(value: T) -> Self {
            Self {
                locked: AtomicBool::new(false),
                value: UnsafeCell::new(value),
            }
        }

        #[inline]
        pub fn lock(&self) -> MutexGuard<'_, T> {
            if self
                .locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                self.lock_slow();
            }
            MutexGuard { mutex: self }
        }

        #[cold]
        fn lock_slow(&self) {
            let mut spin = 4;
            loop {
                if !self.locked.load(Ordering::Relaxed) {
                    if self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                        return;
                    }
                }
                if spin != 0 {
                    spin -= 1;
                    spin_loop_hint();
                } else {
                    extern "C" { fn sched_yield() -> i32; }
                    unsafe { sched_yield() };
                }
            }
        }

        #[inline]
        fn unlock(&self) {
            self.locked.store(false, Ordering::Release);
        }
    }

    pub struct MutexGuard<'a, T> {
        mutex: &'a Mutex<T>,
    }

    impl<'a, T> Drop for MutexGuard<'a, T> {
        fn drop(&mut self) {
            self.mutex.unlock();
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
}

#[cfg(all(windows, target_env = "msvc"))]
mod ntlock {
    use std::{
        cell::UnsafeCell,
        ops::{Deref, DerefMut},
        sync::atomic::{spin_loop_hint, AtomicU32, AtomicU8, AtomicUsize, Ordering},
    };

    const WAKE: u32 = 1 << 8;
    const WAIT: u32 = 1 << 9;

    pub struct Mutex<T> {
        waiters: AtomicU32,
        value: UnsafeCell<T>,
    }

    unsafe impl<T: Send> Send for Mutex<T> {}
    unsafe impl<T: Send> Sync for Mutex<T> {}

    impl<T: Default> Default for Mutex<T> {
        fn default() -> Self {
            Self::new(T::default())
        }
    }

    impl<T> Mutex<T> {
        pub const fn new(value: T) -> Self {
            Self {
                waiters: AtomicU32::new(0),
                value: UnsafeCell::new(value),
            }
        }

        #[inline]
        fn locked(&self) -> &AtomicU8 {
            unsafe { &*(&self.waiters as *const _ as *const _) }
        }

        #[inline]
        pub fn lock(&self) -> MutexGuard<'_, T> {
            if self.locked().swap(1, Ordering::Acquire) != 0 {
                self.lock_slow();
            }
            MutexGuard { mutex: self }
        }

        #[cold]
        fn lock_slow(&self) {
            let handle = Self::get_handle();
            loop {
                let waiters = self.waiters.load(Ordering::Relaxed);
                if waiters & 1 == 0 {
                    if self.locked().swap(1, Ordering::Acquire) == 0 {
                        return;
                    }
                } else if self
                    .waiters
                    .compare_exchange_weak(
                        waiters,
                        (waiters + WAIT) | 1,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    let ret =
                        unsafe { NtWaitForKeyedEvent(handle, self as *const _ as usize, 0, 0) };
                    debug_assert_eq!(ret, 0);
                    self.waiters.fetch_sub(WAKE, Ordering::Relaxed);
                }
                spin_loop_hint();
            }
        }

        fn unlock(&self) {
            self.locked().store(0, Ordering::Release);
            loop {
                let waiters = self.waiters.load(Ordering::Relaxed);
                if (waiters < WAIT) || (waiters & 1 != 0) || (waiters & WAKE != 0) {
                    return;
                }
                if self
                    .waiters
                    .compare_exchange_weak(
                        waiters,
                        (waiters - WAIT) + WAKE,
                        Ordering::Release,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    let ret = unsafe {
                        NtReleaseKeyedEvent(Self::get_handle(), self as *const _ as usize, 0, 0)
                    };
                    debug_assert_eq!(ret, 0);
                    return;
                }
                spin_loop_hint();
            }
        }

        fn handle_ref() -> &'static AtomicUsize {
            static HANDLE: AtomicUsize = AtomicUsize::new(0);
            &HANDLE
        }

        fn get_handle() -> usize {
            let handle = Self::handle_ref().load(Ordering::Relaxed);
            if handle != 0 {
                return handle;
            }
            Self::get_handle_slow()
        }

        #[cold]
        fn get_handle_slow() -> usize {
            let mut handle = 0;
            const MASK: u32 = 0x80000000 | 0x40000000; // GENERIC_READ | GENERIC_WRITE
            let ret = unsafe { NtCreateKeyedEvent(&mut handle, MASK, 0, 0) };
            assert_eq!(ret, 0, "Nt Keyed Events not available");
            match Self::handle_ref().compare_exchange(
                0,
                handle,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => handle,
                Err(new_handle) => {
                    let ret = unsafe { CloseHandle(handle) };
                    debug_assert_eq!(ret, 0);
                    new_handle
                }
            }
        }
    }

    pub struct MutexGuard<'a, T> {
        mutex: &'a Mutex<T>,
    }

    impl<'a, T> Drop for MutexGuard<'a, T> {
        fn drop(&mut self) {
            self.mutex.unlock();
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

    #[link(name = "kernel32")]
    extern "stdcall" {
        fn CloseHandle(handle: usize) -> u32;
    }

    #[link(name = "ntdll")]
    extern "stdcall" {
        fn NtCreateKeyedEvent(handle_ptr: &mut usize, mask: u32, x: usize, y: usize) -> usize;
        fn NtWaitForKeyedEvent(handle: usize, key: usize, block: usize, timeout: usize) -> usize;
        fn NtReleaseKeyedEvent(handle: usize, key: usize, block: usize, timeout: usize) -> usize;
    }
}
