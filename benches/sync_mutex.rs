use std::{iter, sync::Barrier, time};
use crossbeam_utils::{thread::scope, CachePadded};
use criterion::{criterion_group, criterion_main, Benchmark, Criterion};

const TEST_SECS: u64 = 20;
const NUM_OPS: usize = 100;

struct Options {
    n_threads: u32,
    n_locks: u32,
    n_ops: u32,
}

fn run_bench<M: Mutex>(options: &Options) -> time::Duration {
    fn random_numbers(seed: u32) -> impl Iterator<Item = u32> {
        let mut random = seed;
        iter::repeat_with(move || {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            random
        })
    }

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

macro_rules! run_mutex {
    ($b:ident, $mutex:ty, $locks:expr) => {
        use std::convert::TryInto;
        $b.iter(|| {
            run_bench::<$mutex>(&Options {
                n_threads: num_cpus::get().try_into().unwrap(),
                n_locks: $locks,
                n_ops: NUM_OPS.try_into().unwrap(),
            });
        })
    };
}

macro_rules! benchmarks {
    ($c:ident, $mutex:ty) => {
        $c.bench(
            <$mutex>::LABEL,
            Benchmark::new("uncontended", |b| {
                run_mutex!(b, $mutex, 1);
            })
            .with_function("contended", |b| {
                run_mutex!(b, $mutex, 1000);
            }),
        );
    };
}

trait Mutex: Sync + Send + Default {
    const LABEL: &'static str;
    fn with_lock(&self, f: impl FnOnce(&mut u32));
}

fn bench_parking_lot(c: &mut Criterion) {
    impl Mutex for parking_lot::Mutex<u32> {
        const LABEL: &'static str = "parking_lot::Mutex";
        fn with_lock(&self, f: impl FnOnce(&mut u32)) {
            let mut guard = self.lock();
            f(&mut guard)
        }
    }
    benchmarks!(c, parking_lot::Mutex<u32>);
}

fn bench_std_mutex(c: &mut Criterion) {
    impl Mutex for std::sync::Mutex<u32> {
        const LABEL: &'static str = "std::sync::Mutex";
        fn with_lock(&self, f: impl FnOnce(&mut u32)) {
            let mut guard = self.lock().unwrap();
            f(&mut guard)
        }
    }
    benchmarks!(c, std::sync::Mutex<u32>);
}

fn bench_yaar(c: &mut Criterion) {
    impl Mutex for yaar::sync::Mutex<u32> {
        const LABEL: &'static str = "yaar::sync::Mutex";
        fn with_lock(&self, f: impl FnOnce(&mut u32)) {
            let mut guard = self.lock();
            f(&mut guard)
        }
    }
    benchmarks!(c, yaar::sync::Mutex<u32>);
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .measurement_time(time::Duration::from_secs(TEST_SECS));
    targets = 
        bench_yaar,
        bench_parking_lot,
        bench_std_mutex
}
criterion_main!(benches);
