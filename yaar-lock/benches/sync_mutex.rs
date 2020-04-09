#[macro_use]
extern crate criterion;

use criterion::Criterion;
use std::{
    mem::drop,
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};
use yaar_lock::utils::CachePadded;

mod sync_mutexes;
use sync_mutexes::*;

type BenchValue = CachePadded<f64>;

#[derive(Copy, Clone)]
struct BenchContext {
    num_threads: usize,
    work_per_critical_section: usize,
}

fn bench_mutex<M>(c: &mut Criterion, ctx: BenchContext)
where
    M: Mutex<BenchValue> + Send + Sync + 'static
{
    c.bench_function(
        &format!(
            "[sync_mutex] [throughput] {} threads={} work_per_lock={}",
            M::NAME,
            ctx.num_threads,
            ctx.work_per_critical_section,
        ),
        |b| {
            b.iter_custom(|iters| {
                let mutex = Arc::new(M::new(BenchValue::new(0.0)));
                let barrier = Arc::new(Barrier::new(ctx.num_threads + 1));

                let threads = (0..ctx.num_threads)
                    .map(|_| {
                        let mutex = mutex.clone();
                        let barrier = barrier.clone();
                        thread::spawn(move || {
                            barrier.wait();
                            let mut local_value = 0.0;
                            for _ in 0..iters {
                                mutex.locked(|shared_value| {
                                    for _ in 0..ctx.work_per_critical_section {
                                        **shared_value += local_value;
                                        **shared_value *= 1.01;
                                        local_value = **shared_value;
                                    }
                                });
                            }
                            local_value
                        })
                    })
                    .collect::<Vec<_>>();

                let start = Instant::now();
                barrier.wait();
                threads
                    .into_iter()
                    .map(|t| t.join().unwrap())
                    .for_each(drop);
                start.elapsed()
            })
        },
    );
}

fn bench_all(c: &mut Criterion, ctx: BenchContext) {
    bench_mutex::<std::sync::Mutex<BenchValue>>(c, ctx);
    bench_mutex::<parking_lot::Mutex<BenchValue>>(c, ctx);
    bench_mutex::<yaar_lock::sync::Mutex<BenchValue>>(c, ctx);

    bench_mutex::<std_lock::Mutex<BenchValue>>(c, ctx);
    bench_mutex::<spin_lock::Mutex<BenchValue>>(c, ctx);
    bench_mutex::<sys_lock::Mutex<BenchValue>>(c, ctx);

    #[cfg(windows)]
    bench_mutex::<nt_lock::Mutex<BenchValue>>(c, ctx);
}

fn bench_throughput(c: &mut Criterion, work_per_critical_section: usize) {
    let max_threads = num_cpus::get();

    let mut last_tested = 0;
    let mut num_threads = 1;
    while num_threads < max_threads / 2 {
        last_tested = num_threads;
        bench_all(
            c,
            BenchContext {
                num_threads,
                work_per_critical_section,
            },
        );
        if num_threads < 2 {
            num_threads += 1;
        } else {
            num_threads *= 2;
        }
    }

    if last_tested < max_threads / 2 {
        bench_all(
            c,
            BenchContext {
                num_threads: max_threads / 2,
                work_per_critical_section,
            },
        );
    }

    bench_all(
        c,
        BenchContext {
            num_threads: max_threads,
            work_per_critical_section,
        },
    );
    bench_all(
        c,
        BenchContext {
            num_threads: max_threads * 2,
            work_per_critical_section,
        },
    );
}

fn no_critical_section(c: &mut Criterion) {
    bench_throughput(c, 1);
}

fn small_critical_section(c: &mut Criterion) {
    bench_throughput(c, 10);
}

fn large_critical_section(c: &mut Criterion) {
    bench_throughput(c, 20);
}

const NOISE_PERCENT: f64 = 0.10;
const WARM_UP: Duration = Duration::from_millis(500);
const MEASURE: Duration = Duration::from_millis(500);

criterion_group! {
    name = lock_throughput;
    config = Criterion::default()
        .noise_threshold(NOISE_PERCENT)
        .warm_up_time(WARM_UP)
        .measurement_time(MEASURE);
    targets = no_critical_section
}

criterion_group! {
    name = lock_nonblocking;
    config = Criterion::default()
        .noise_threshold(NOISE_PERCENT)
        .warm_up_time(WARM_UP)
        .measurement_time(MEASURE);
    targets = small_critical_section
}

criterion_group! {
    name = lock_blocking;
    config = Criterion::default()
        .noise_threshold(NOISE_PERCENT)
        .warm_up_time(WARM_UP)
        .measurement_time(MEASURE);
    targets = large_critical_section
}

criterion_main!(lock_nonblocking, lock_blocking,);
