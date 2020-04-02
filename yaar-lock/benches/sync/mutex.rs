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
        const NAME: &'static str = "sched_yield";

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

#[cfg(windows)]
mod sleep_lock {
    use std::{
        cell::UnsafeCell,
        sync::atomic::{Ordering, AtomicBool},
    };

    pub struct Mutex<T> {
        locked: AtomicBool,
        value: UnsafeCell<T>,
    }

    unsafe impl<T> Sync for Mutex<T> {}

    impl<T> super::Mutex<T> for Mutex<T> {
        const NAME: &'static str = "Sleep(0)";

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
            loop {
                if !self.locked.load(Ordering::Relaxed) {
                    if self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                        return;
                    }
                }

                #[link(name = "kernel32")]
                extern "stdcall" { fn Sleep(ms: u32); }
                unsafe { Sleep(0) };
            }
        }
    }
}

#[cfg(windows)]
mod yield_lock {
    use std::{
        cell::UnsafeCell,
        sync::atomic::{Ordering, AtomicBool},
    };

    pub struct Mutex<T> {
        locked: AtomicBool,
        value: UnsafeCell<T>,
    }

    unsafe impl<T> Sync for Mutex<T> {}

    impl<T> super::Mutex<T> for Mutex<T> {
        const NAME: &'static str = "SwitchToThread";

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
            loop {
                if !self.locked.load(Ordering::Relaxed) {
                    if self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                        return;
                    }
                }

                #[link(name = "kernel32")]
                extern "stdcall" { fn SwitchToThread() -> u32; }
                unsafe { let _ = SwitchToThread(); };
            }
        }
    }
}

#[cfg(windows)]
mod ntlock {
    use std::{
        cell::UnsafeCell,
        mem::transmute,
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
            if self.locked().load(Ordering::Relaxed) == 0 {
                if self
                    .locked()
                    .compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed)
                    .is_err()
                {
                    self.lock_slow();
                }
            } else {
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
                    if self
                        .locked()
                        .compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed)
                        .is_ok()
                    {
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
                    unsafe {
                        let wait: NtInvoke = transmute(NT_WAIT.load(Ordering::Relaxed));
                        let status = wait(handle, self as *const _ as usize, 0, 0);
                        debug_assert_eq!(status, 0, "NtWaitForKeyedEvent failed");
                        self.waiters.fetch_sub(WAKE, Ordering::Relaxed);
                    }
                }
                
            }
        }

        #[inline]
        fn unlock(&self) {
            self.locked().store(0, Ordering::Release);
            self.unlock_slow();
        }

        #[cold]
        fn unlock_slow(&self) {
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
                    return unsafe {
                        let handle = Self::get_handle();
                        let notify: NtInvoke = transmute(NT_NOTIFY.load(Ordering::Relaxed));
                        let status = notify(handle, self as *const _ as usize, 0, 0);
                        debug_assert_eq!(status, 0, "NtReleaseKeyedEvent failed");
                    };
                }
                spin_loop_hint();
            }
        }

        fn get_handle() -> usize {
            let handle = NT_HANDLE.load(Ordering::Acquire);
            if handle != 0 {
                return handle;
            }
            Self::get_handle_slow()
        }

        #[cold]
        fn get_handle_slow() -> usize {
            unsafe {
                let dll = GetModuleHandleW((&[
                    b'n' as u16,
                    b't' as u16,
                    b'd' as u16,
                    b'l' as u16,
                    b'l' as u16,
                    b'.' as u16,
                    b'd' as u16,
                    b'l' as u16,
                    b'l' as u16,
                    0 as u16,
                ]).as_ptr());
                assert_ne!(dll, 0, "Failed to load ntdll.dll");

                let notify = GetProcAddress(dll, b"NtReleaseKeyedEvent\0".as_ptr());
                assert_ne!(notify, 0, "Failed to load NtReleaseKeyedEvent");
                NT_NOTIFY.store(notify, Ordering::Relaxed);

                let wait = GetProcAddress(dll, b"NtWaitForKeyedEvent\0".as_ptr());
                assert_ne!(wait, 0, "Failed to load NtWaitForKeyedEvent");
                NT_WAIT.store(wait, Ordering::Relaxed);

                let create = GetProcAddress(dll, b"NtCreateKeyedEvent\0".as_ptr());
                assert_ne!(create, 0, "Failed to load NtCreateKeyedEvent");
                let create: NtCreate = transmute(create);

                let mut handle = 0;
                let status = create(&mut handle, 0x80000000 | 0x40000000, 0, 0);
                assert_eq!(status, 0, "Failed to create NT Keyed Event Handle");

                match NT_HANDLE.compare_exchange(
                    0,
                    handle,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => handle,
                    Err(new_handle) => {
                        let status = CloseHandle(handle);
                        assert_eq!(status, 1, "Failed to close extra NT Keyed Event Handle");
                        new_handle
                    }
                }
            }
        }
    }

    static NT_WAIT: AtomicUsize = AtomicUsize::new(0);
    static NT_NOTIFY: AtomicUsize = AtomicUsize::new(0);
    static NT_HANDLE: AtomicUsize = AtomicUsize::new(0);

    type NtCreate = extern "stdcall" fn(
        handle: &mut usize,
        mask: u32,
        unused: usize,
        unused: usize,
    ) -> u32;
    type NtInvoke = extern "stdcall" fn(
        handle: usize,
        key: usize,
        block: usize,
        timeout: usize,
    ) -> u32;

    #[link(name = "kernel32")]
    extern "stdcall" {
        fn CloseHandle(handle: usize) -> i32;
        fn GetModuleHandleW(moduleName: *const u16) -> usize;
        fn GetProcAddress(module: usize, procName: *const u8) -> usize;
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

mod std_lock;
impl<T> Mutex<T> for std_lock::Mutex<T> {
    const NAME: &'static str = "std_lock";

    fn new(v: T) -> Self {
        Self::new(v)
    }

    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        let mut guard = self.lock();
        f(&mut *guard)
    }
}

mod prot_lock;
impl<T> Mutex<T> for prot_lock::Mutex<T> {
    const NAME: &'static str = "prot_lock";

    fn new(v: T) -> Self {
        Self::new(v)
    }

    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        let mut guard = self.lock();
        f(&mut *guard)
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

    run_benchmark_iterations::<std_lock::Mutex<f64>>(
        num_threads,
        work_per_critical_section,
        work_between_critical_sections,
        seconds_per_test,
        test_iterations,
    );

    run_benchmark_iterations::<prot_lock::Mutex<f64>>(
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

    #[cfg(windows)] {
        run_benchmark_iterations::<ntlock::Mutex<f64>>(
            num_threads,
            work_per_critical_section,
            work_between_critical_sections,
            seconds_per_test,
            test_iterations,
        );
        run_benchmark_iterations::<sleep_lock::Mutex<f64>>(
            num_threads,
            work_per_critical_section,
            work_between_critical_sections,
            seconds_per_test,
            test_iterations,
        );
        run_benchmark_iterations::<yield_lock::Mutex<f64>>(
            num_threads,
            work_per_critical_section,
            work_between_critical_sections,
            seconds_per_test,
            test_iterations,
        );
    }

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
    bench_all("Extreme Contention", num_threads * 2);
    bench_all("High Contention", num_threads);
    if num_threads > 3 {
        bench_all("Some Contention", num_threads / 2);
    }
    bench_all("Uncontended", 1);
}