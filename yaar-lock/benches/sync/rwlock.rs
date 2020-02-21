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

trait RwLock<T> {
    const NAME: &'static str;

    fn new(v: T) -> Self;

    fn read<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&T) -> R;

    fn write<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R;
}

impl<T> RwLock<T> for std::sync::RwLock<T> {
    const NAME: &'static str = "std::sync::RwLock";

    fn new(v: T) -> Self {
        Self::new(v)
    }

    fn read<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        f(&*self.read().unwrap())
    }

    fn write<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.write().unwrap())
    }
}

impl<T> RwLock<T> for parking_lot::RwLock<T> {
    const NAME: &'static str = "parking_lot::RwLock";

    fn new(v: T) -> Self {
        Self::new(v)
    }

    fn read<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        f(&*self.read())
    }

    fn write<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.write())
    }
}

impl<T> RwLock<T> for yaar_lock::sync::RwLock<T> {
    const NAME: &'static str = "yaar_lock::RwLock";

    fn new(v: T) -> Self {
        Self::new(v)
    }

    fn read<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        f(&*self.read())
    }

    fn write<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.write())
    }
}

#[cfg(unix)]
mod pthread {
    use std::cell::UnsafeCell;

    pub struct RwLock<T> {
        value: UnsafeCell<T>,
        lock: UnsafeCell<libc::pthread_rwlock_t>,
    }

    unsafe impl<T> Sync for RwLock<T> {}

    impl<T> Drop for RwLock<T> {
        fn drop(&mut self) {
            unsafe {
                libc::pthread_rwlock_destroy(self.lock.get());
            }
        }
    }

    impl<T> super::RwLock<T> for RwLock<T> {
        const NAME: &'static str = "pthread_rwlock_t";

        fn new(v: T) -> Self {
            Self {
                value: UnsafeCell::new(v),
                lock: UnsafeCell::new(libc::PTHREAD_RWLOCK_INITIALIZER),
            }
        }

        fn read<F, R>(&self, f: F) -> R
        where
            F: FnOnce(&T) -> R,
        {
            unsafe {
                libc::pthread_rwlock_rdlock(self.lock.get());
                let res = f(&*self.value.get());
                libc::pthread_rwlock_unlock(self.lock.get());
                res
            }
        }
    
        fn write<F, R>(&self, f: F) -> R
        where
            F: FnOnce(&mut T) -> R,
        {
            unsafe {
                libc::pthread_rwlock_wrlock(self.lock.get());
                let res = f(&mut *self.value.get());
                libc::pthread_rwlock_unlock(self.lock.get());
                res
            }
        }
    }
}

fn run_benchmark<M: RwLock<f64> + Send + Sync + 'static>(
    num_writer_threads: usize,
    num_reader_threads: usize,
    work_per_critical_section: usize,
    work_between_critical_sections: usize,
    seconds_per_test: usize,
) -> (Vec<usize>, Vec<usize>) {
    let lock = Arc::new(([0u8; 300], M::new(0.0), [0u8; 300]));
    let keep_going = Arc::new(AtomicBool::new(true));
    let barrier = Arc::new(Barrier::new(num_reader_threads + num_writer_threads));
    let mut writers = vec![];
    let mut readers = vec![];
    for _ in 0..num_writer_threads {
        let barrier = barrier.clone();
        let lock = lock.clone();
        let keep_going = keep_going.clone();
        writers.push(thread::spawn(move || {
            let mut local_value = 0.0;
            let mut value = 0.0;
            let mut iterations = 0usize;
            barrier.wait();
            while keep_going.load(Ordering::Relaxed) {
                lock.1.write(|shared_value| {
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
    for _ in 0..num_reader_threads {
        let barrier = barrier.clone();
        let lock = lock.clone();
        let keep_going = keep_going.clone();
        readers.push(thread::spawn(move || {
            let mut local_value = 0.0;
            let mut value = 0.0;
            let mut iterations = 0usize;
            barrier.wait();
            while keep_going.load(Ordering::Relaxed) {
                lock.1.read(|shared_value| {
                    for _ in 0..work_per_critical_section {
                        local_value += value;
                        local_value *= *shared_value;
                        value = local_value;
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

    thread::sleep(Duration::new(seconds_per_test as u64, 0));
    keep_going.store(false, Ordering::Relaxed);

    let run_writers = writers
        .into_iter()
        .map(|x| x.join().unwrap().0)
        .collect::<Vec<usize>>();
    let run_readers = readers
        .into_iter()
        .map(|x| x.join().unwrap().0)
        .collect::<Vec<usize>>();

    (run_writers, run_readers)
}

fn run_benchmark_iterations<M: RwLock<f64> + Send + Sync + 'static>(
    num_writer_threads: usize,
    num_reader_threads: usize,
    work_per_critical_section: usize,
    work_between_critical_sections: usize,
    seconds_per_test: usize,
    test_iterations: usize,
) {
    let mut writers = vec![];
    let mut readers = vec![];

    for _ in 0..test_iterations {
        let (run_writers, run_readers) = run_benchmark::<M>(
            num_writer_threads,
            num_reader_threads,
            work_per_critical_section,
            work_between_critical_sections,
            seconds_per_test,
        );
        writers.extend_from_slice(&run_writers);
        readers.extend_from_slice(&run_readers);
    }

    let total_writers = writers.iter().fold(0f64, |a, b| a + *b as f64) / test_iterations as f64;
    let total_readers = readers.iter().fold(0f64, |a, b| a + *b as f64) / test_iterations as f64;
    println!(
        "{:20} - [write] {:10.3} kHz [read] {:10.3} kHz",
        M::NAME,
        total_writers as f64 / seconds_per_test as f64 / 1000.0,
        total_readers as f64 / seconds_per_test as f64 / 1000.0
    );
}

fn run_all(
    num_writer_threads: usize,
    num_reader_threads: usize,
    work_per_critical_section: usize,
    work_between_critical_sections: usize,
    seconds_per_test: usize,
    test_iterations: usize,
) {
    run_benchmark_iterations::<parking_lot::RwLock<f64>>(
        num_writer_threads,
        num_reader_threads,
        work_per_critical_section,
        work_between_critical_sections,
        seconds_per_test,
        test_iterations,
    );

    run_benchmark_iterations::<yaar_lock::sync::RwLock<f64>>(
        num_writer_threads,
        num_reader_threads,
        work_per_critical_section,
        work_between_critical_sections,
        seconds_per_test,
        test_iterations,
    );

    run_benchmark_iterations::<std::sync::RwLock<f64>>(
        num_writer_threads,
        num_reader_threads,
        work_per_critical_section,
        work_between_critical_sections,
        seconds_per_test,
        test_iterations,
    );
    if cfg!(unix) {
        run_benchmark_iterations::<pthread::RwLock<f64>>(
            num_writer_threads,
            num_reader_threads,
            work_per_critical_section,
            work_between_critical_sections,
            seconds_per_test,
            test_iterations,
        );
    }
}

fn bench_all(name: &'static str, readers: usize, writers: usize) {
    println!("-- {} (readers: {} writers: {})", name, readers, writers);
    let work_per_critical_section = 0;
    let work_between_critical_sections = 0;
    let seconds_per_test = 1;
    let test_iterations = 1;

    run_all(
        writers,
        readers,
        work_per_critical_section,
        work_between_critical_sections,
        seconds_per_test,
        test_iterations,
    );
    println!();
}

fn main() {
    let max_threads = num_cpus::get();

    /*
    bench_all("Only Readers - Contention", max_threads, 0);
    bench_all("Only Readers - Uncontended", 1, 0);
    */

    bench_all("Only Writers - Contention", 0, max_threads);
    bench_all("Only Writers - Uncontended", 0, 1);

    /*
    bench_all("Many Readers - One Writer", max_threads.max(2), 1);
    bench_all("Many Writers - One Reader", 1, max_threads.max(2));

    bench_all("Equal Readers & Writers", max_threads, max_threads);
    */
}