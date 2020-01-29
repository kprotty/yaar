use std::{time::Duration, iter, sync::Barrier};
use crossbeam_utils::{CachePadded, thread::scope};
use criterion::{criterion_group, criterion_main, Benchmark, Criterion};

const NUM_OPS: usize = 100;
const TEST_SECS: u64 = 10;

trait Mutex: Sync + Send + Default {
    fn with_lock(&self, is_fair: bool, f: impl FnOnce(&mut usize));
}

fn run_mutex<M: Mutex>(num_locks: usize, is_fair: bool) {
    let random_numbers = |seed| {
        let mut random = seed;
        iter::repeat_with(move || {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            random
        })
    };

    let locks = &(0..num_locks)
        .map(|_| CachePadded::new(M::default()))
        .collect::<Vec<_>>();

    let num_threads = num_cpus::get();
    let stop_barrier = &Barrier::new(num_threads + 1);
    let start_barrier = &Barrier::new(num_threads + 1);

    scope(|scope| {
        let _ = random_numbers(0x6F4A955E)
            .scan(0x9BA2BF27, |state, n| {
                *state ^= n;
                Some(*state)
            })
            .take(num_threads)
            .for_each(|thread_seed| {
                scope.spawn(move |_| {
                    start_barrier.wait();
                    random_numbers(thread_seed)
                        .map(|it| it % num_locks)
                        .take(NUM_OPS)
                        .for_each(|index| {
                            locks[index].with_lock(is_fair, |c| *c += 1)
                        });
                    stop_barrier.wait();
                });
            });

        start_barrier.wait();
        stop_barrier.wait();

        let mut total = 0;
        locks.iter().for_each(|lock| lock.with_lock(is_fair, |c| total += *c));
        assert_eq!(num_threads * NUM_OPS, total);
    }).unwrap();
}

type Std = std::sync::Mutex<usize>;
impl Mutex for Std {
    fn with_lock(&self, _is_fair: bool, f: impl FnOnce(&mut usize)) {
        let mut guard = self.lock().unwrap();
        f(&mut guard);
    }
}

type ParkingLot = parking_lot::Mutex<usize>;
impl Mutex for ParkingLot {
    fn with_lock(&self, is_fair: bool, f: impl FnOnce(&mut usize)) {
        let mut guard = self.lock();
        f(&mut guard);
        if is_fair {
            parking_lot::MutexGuard::<'_, usize>::unlock_fair(guard)
        } else {
            std::mem::drop(guard)
        }
    }
}

type YaarLock = yaar_lock::sync::Mutex<usize>;
impl Mutex for YaarLock {
    fn with_lock(&self, is_fair: bool, f: impl FnOnce(&mut usize)) {
        let mut guard = self.lock();
        f(&mut guard);
        if is_fair {
            yaar_lock::sync::MutexGuard::<'_, usize>::unlock_fair(guard)
        } else {
            std::mem::drop(guard)
        }
    }
}

macro_rules! benchmarks {
    ($c:expr, $Mutex:ty, $name:literal, $fair:literal) => {
        $c.bench(
            $name,
            Benchmark::new("contention", |b| {
                b.iter(|| run_mutex::<$Mutex>(100000, $fair));
            })
            .with_function("no_contention", |b| {
                b.iter(|| run_mutex::<$Mutex>(1, $fair));
            }),
        )
    };
}

fn bench_std_mutex(c: &mut Criterion) {
    benchmarks!(c, Std, "std::sync::Mutex", false);
}

fn bench_parking_lot(c: &mut Criterion) {
    benchmarks!(c, ParkingLot, "parking_lot::Mutex", false);
}

fn bench_parking_lot_fair(c: &mut Criterion) {
    benchmarks!(c, ParkingLot, "parking_lot::Mutex(fair)", true);
}

fn bench_yaar_lock(c: &mut Criterion) {
    benchmarks!(c, YaarLock, "yaar_lock::sync::Mutex", false);
}

fn bench_yaar_lock_fair(c: &mut Criterion) {
    benchmarks!(c, YaarLock, "yaar_lock::sync::Mutex(fair)", true);
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(20)
        .measurement_time(Duration::from_secs(TEST_SECS));
    targets =
        bench_yaar_lock,
        bench_parking_lot,
        bench_yaar_lock_fair,
        bench_parking_lot_fair,
        bench_std_mutex,
}
criterion_main!(benches);