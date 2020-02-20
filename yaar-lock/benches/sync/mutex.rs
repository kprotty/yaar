// Copyright 2016 Amanieu d'Antras
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Barrier,
    },
    thread,
    time::Duration,
};

trait Mutex<T> {
    const NAME: &'static str;

    fn new(v: T) -> Self;

    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R;
}

impl<T> Mutex<T> for std::sync::Mutex<T> {
    const NAME: &'static str = "std::sync::Mutex";

    fn new(v: T) -> Self {
        Self::new(v)
    }

    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.lock().unwrap())
    }
}

impl<T> Mutex<T> for parking_lot::Mutex<T> {
    const NAME: &'static str = "parking_lot::Mutex";

    fn new(v: T) -> Self {
        Self::new(v)
    }

    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.lock())
    }
}

impl<T> Mutex<T> for yaar_lock::sync::Mutex<T> {
    const NAME: &'static str = "yaar_lock::Mutex";

    fn new(v: T) -> Self {
        Self::new(v)
    }

    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.lock())
    }
}

#[cfg(unix)]
mod pthread {
    use std::cell::UnsafeCell;

    pub struct Mutex<T> {
        value: UnsafeCell<T>,
        lock: UnsafeCell<libc::pthread_mutex_t>,
    }

    unsafe impl<T> Sync for Mutex<T> {}

    impl<T> Drop for Mutex<T> {
        fn drop(&mut self) {
            unsafe {
                libc::pthread_mutex_destroy(self.lock.get());
            }
        }
    }

    impl<T> super::Mutex<T> for Mutex<T> {
        const NAME: &'static str = "pthread_rwlock_t";

        fn new(v: T) -> Self {
            Self {
                value: UnsafeCell::new(v),
                lock: UnsafeCell::new(libc::PTHREAD_MUTEX_INITIALIZER),
            }
        }

        fn lock<F, R>(&self, f: F) -> R
        where
            F: FnOnce(&mut T) -> R,
        {
            unsafe {
                libc::pthread_mutex_lock(self.lock.get());
                let res = f(&mut *self.value.get());
                libc::pthread_mutex_unlock(self.lock.get());
                res
            }
        }
    }
}

#[cfg(unix)]
mod posix_lock {
    use std::{
        cell::UnsafeCell,
        sync::atomic::{spin_loop_hint, Ordering, AtomicBool},
    };

    pub struct Mutex<T> {
        locked: AtomicBool,
        value: UnsafeCell<T>,
    }

    unsafe impl<T> Sync for Mutex<T> {}

    impl<T> super::Mutex<T> for Mutex<T> {
        const NAME: &'static str = "spin::sched_yield";

        fn new(v: T) -> Self {
            Self {
                locked: AtomicBool::new(false),
                value: UnsafeCell::new(v),
            }
        }

        fn lock<F, R>(&self, f: F) -> R
        where
            F: FnOnce(&mut T) -> R,
        {
            if self
                .locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                self.lock_slow();
            }
            let res = f(unsafe { &mut *self.value.get() });
            self.locked.store(false, Ordering::Release);
            res
        }
    }

    impl<T> Mutex<T> {
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
                    let _ = unsafe { libc::sched_yield() };
                }
            }
        }
    }
}

#[cfg(all(windows, target_env = "msvc"))]
mod ntlock {
    use std::{
        cell::UnsafeCell,
        sync::atomic::{spin_loop_hint, AtomicU32, AtomicU8, AtomicUsize, Ordering},
    };

    const WAKE: u32 = 1 << 8;
    const WAIT: u32 = 1 << 9;

    pub struct Mutex<T> {
        waiters: AtomicU32,
        value: UnsafeCell<T>,
    }

    unsafe impl<T: Send> Sync for Mutex<T> {}

    impl<T> super::Mutex<T> for Mutex<T> {
        const NAME: &'static str = "NtKeyedEvent";

        fn new(v: T) -> Self {
            Self {
                waiters: AtomicU32::new(0),
                value: UnsafeCell::new(v),
            }
        }

        fn lock<F, R>(&self, f: F) -> R
        where
            F: FnOnce(&mut T) -> R,
        {
            if self.locked().swap(1, Ordering::Acquire) != 0 {
                self.lock_slow();
            }
            let res = f(unsafe { &mut *self.value.get() });
            self.unlock();
            res
        }
    }

    impl<T> Mutex<T> {
        #[inline]
        fn locked(&self) -> &AtomicU8 {
            unsafe { &*(&self.waiters as *const _ as *const _) }
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

mod spinlock {
    use std::{
        cell::UnsafeCell,
        sync::atomic::{spin_loop_hint, Ordering, AtomicBool},
    };

    pub struct Mutex<T> {
        locked: AtomicBool,
        value: UnsafeCell<T>,
    }

    unsafe impl<T> Sync for Mutex<T> {}

    impl<T> super::Mutex<T> for Mutex<T> {
        const NAME: &'static str = "spin_lock";

        fn new(v: T) -> Self {
            Self {
                locked: AtomicBool::new(false),
                value: UnsafeCell::new(v),
            }
        }

        fn lock<F, R>(&self, f: F) -> R
        where
            F: FnOnce(&mut T) -> R,
        {
            if self
                .locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                self.lock_slow();
            }
            let res = f(unsafe { &mut *self.value.get() });
            self.locked.store(false, Ordering::Release);
            res
        }
    }

    impl<T> Mutex<T> {
        #[cold]
        fn lock_slow(&self) {
            let mut spin = 0;
            loop {
                if !self.locked.load(Ordering::Relaxed) {
                    if self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                        return;
                    }
                }
                (0..(1 << spin)).for_each(|_| spin_loop_hint());
                if spin <= 6 {
                    spin += 1;
                }
            }
        }
    }
}

fn run_benchmark<M: Mutex<f64> + Send + Sync + 'static>(
    num_threads: usize,
    work_per_critical_section: usize,
    work_between_critical_sections: usize,
    seconds_per_test: usize,
) -> Vec<usize> {
    let lock = Arc::new(([0u8; 300], M::new(0.0), [0u8; 300]));
    let keep_going = Arc::new(AtomicBool::new(true));
    let barrier = Arc::new(Barrier::new(num_threads));
    let mut threads = vec![];
    for _ in 0..num_threads {
        let barrier = barrier.clone();
        let lock = lock.clone();
        let keep_going = keep_going.clone();
        threads.push(thread::spawn(move || {
            let mut local_value = 0.0;
            let mut value = 0.0;
            let mut iterations = 0usize;
            barrier.wait();
            while keep_going.load(Ordering::Relaxed) {
                lock.1.lock(|shared_value| {
                    for _ in 0..work_per_critical_section {
                        *shared_value += value;
                        *shared_value *= 1.01;
                        value = *shared_value;
                    }
                });
                for _ in 0..work_between_critical_sections {
                    local_value += value;
                    local_value *= 1.01;
                    value = local_value;
                }
                iterations += 1;
            }
            (iterations, value)
        }));
    }

    thread::sleep(Duration::from_secs(seconds_per_test as u64));
    keep_going.store(false, Ordering::Relaxed);
    threads.into_iter().map(|x| x.join().unwrap().0).collect()
}

fn run_benchmark_iterations<M: Mutex<f64> + Send + Sync + 'static>(
    num_threads: usize,
    work_per_critical_section: usize,
    work_between_critical_sections: usize,
    seconds_per_test: usize,
    test_iterations: usize,
) {
    let mut data = vec![];
    for _ in 0..test_iterations {
        let run_data = run_benchmark::<M>(
            num_threads,
            work_per_critical_section,
            work_between_critical_sections,
            seconds_per_test,
        );
        data.extend_from_slice(&run_data);
    }

    let average = data.iter().fold(0f64, |a, b| a + *b as f64) / data.len() as f64;
    let variance = data
        .iter()
        .fold(0f64, |a, b| a + ((*b as f64 - average).powi(2)))
        / data.len() as f64;
    data.sort();

    let k_hz = 1.0 / seconds_per_test as f64 / 1000.0;
    println!(
        "{:20} | {:10.3} kHz | {:10.3} kHz | {:10.3} kHz",
        M::NAME,
        average * k_hz,
        data[data.len() / 2] as f64 * k_hz,
        variance.sqrt() * k_hz
    );
}

fn run_all(
    num_threads: usize,
    work_per_critical_section: usize,
    work_between_critical_sections: usize,
    seconds_per_test: usize,
    test_iterations: usize,
) {
    println!(
        "{:^20} | {:^14} | {:^14} | {:^14}",
        "name", "average", "median", "std.dev."
    );

    run_benchmark_iterations::<parking_lot::Mutex<f64>>(
        num_threads,
        work_per_critical_section,
        work_between_critical_sections,
        seconds_per_test,
        test_iterations,
    );

    run_benchmark_iterations::<yaar_lock::sync::Mutex<f64>>(
        num_threads,
        work_per_critical_section,
        work_between_critical_sections,
        seconds_per_test,
        test_iterations,
    );

    run_benchmark_iterations::<std::sync::Mutex<f64>>(
        num_threads,
        work_per_critical_section,
        work_between_critical_sections,
        seconds_per_test,
        test_iterations,
    );

    #[cfg(unix)] {
        run_benchmark_iterations::<pthread::Mutex<f64>>(
            num_threads,
            work_per_critical_section,
            work_between_critical_sections,
            seconds_per_test,
            test_iterations,
        );
        run_benchmark_iterations::<posix_lock::Mutex<f64>>(
            num_threads,
            work_per_critical_section,
            work_between_critical_sections,
            seconds_per_test,
            test_iterations,
        );
    }

    #[cfg(all(windows, target_env = "msvc"))]
    run_benchmark_iterations::<ntlock::Mutex<f64>>(
        num_threads,
        work_per_critical_section,
        work_between_critical_sections,
        seconds_per_test,
        test_iterations,
    );

    run_benchmark_iterations::<spinlock::Mutex<f64>>(
        num_threads,
        work_per_critical_section,
        work_between_critical_sections,
        seconds_per_test,
        test_iterations,
    );
}

fn bench_all(name: &'static str, num_threads: usize) {
    let work_per_critical_section = 0;
    let work_between_critical_sections = 0;
    let seconds_per_test = 1;
    let test_iterations = 1;

    println!("-- {} (num_threads = {})", name, num_threads);
    run_all(
        num_threads,
        work_per_critical_section,
        work_between_critical_sections,
        seconds_per_test,
        test_iterations,
    );
    println!();
}

fn main() {
    let num_threads = num_cpus::get();
    bench_all("High Contention", num_threads);
    if num_threads > 3 {
        bench_all("Some Contention", num_threads / 2);
    }
    bench_all("Uncontended", 1);
}