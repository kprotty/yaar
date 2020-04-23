#[macro_use]
extern crate criterion;

use criterion::Criterion;
use std::{
    convert::TryInto,
    mem::drop,
    sync::{Arc, Barrier},
    thread,
    time::{Instant, Duration},
};
// use yaar_lock::event::OsInstant as Instant;
use yaar_lock::utils::CachePadded;

mod sync_mutexes;
use sync_mutexes::*;

type BenchValue = CachePadded<f64>;

#[derive(Copy, Clone, Eq, PartialEq)]
enum BenchType {
    Throughput,
    Latency,
}

#[derive(Copy, Clone)]
struct BenchContext {
    num_threads: usize,
    bench_type: BenchType,
    work_per_critical_section: usize,
}

fn run_bench<M, T, U>(
    ctx: BenchContext,
    iters: u64,
    with_iters: impl Fn(Arc<M>, u64, usize) -> T + Send + 'static + Copy,
    with_threads: impl FnOnce(Arc<Barrier>, Vec<thread::JoinHandle<T>>) -> U,
) -> U
where
    M: Mutex<BenchValue> + Send + Sync + 'static,
    T: Send + 'static,
{
    let mutex = Arc::new(M::new(BenchValue::new(0.0)));
    let barrier = Arc::new(Barrier::new(ctx.num_threads + 1));
    let threads = (0..ctx.num_threads)
        .map(|_| {
            let mutex = mutex.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                with_iters(mutex, iters, ctx.work_per_critical_section)
            })
        })
        .collect::<Vec<_>>();
    with_threads(barrier, threads)
}

fn bench_throughput<M>(ctx: BenchContext, iters: u64) -> Duration
where
    M: Mutex<BenchValue> + Send + Sync + 'static,
{
    run_bench(
        ctx,
        iters,
        move |mutex: Arc<M>, iters, work| {
            let mut local_value = 0.0;
            for _ in 0..iters {
                mutex.locked(|shared_value| {
                    for _ in 0..work {
                        **shared_value += local_value;
                        **shared_value *= 1.01;
                        local_value = **shared_value;
                    }
                });
            }
            local_value
        },
        move |barrier, threads| {
            let start = Instant::now();
            barrier.wait();
            threads
                .into_iter()
                .map(|t| t.join().unwrap())
                .for_each(drop);
            start.elapsed()
        },
    )
}

fn bench_latency<M>(ctx: BenchContext, iters: u64) -> Duration
where
    M: Mutex<BenchValue> + Send + Sync + 'static,
{
    run_bench(
        ctx,
        iters,
        move |mutex: Arc<M>, iters, work| {
            let mut local_value = 0.0;
            let mut avg_latency = 0u128;
            for i in 0..(iters as u128) {
                let start = Instant::now();
                let elapsed = mutex.locked(|shared_value| {
                    let elapsed = start.elapsed().as_nanos();
                    for _ in 0..work {
                        **shared_value += local_value;
                        **shared_value *= 1.01;
                        local_value = **shared_value;
                    }
                    elapsed
                });
                if elapsed != 0 {
                    avg_latency = ((avg_latency * i) + elapsed) / (i + 1);
                }
            }
            (avg_latency, local_value)
        },
        move |barrier, threads| {
            barrier.wait();
            let avg_latency = threads
                .into_iter()
                .map(|t| t.join().unwrap().0)
                .enumerate()
                .fold(0u128, |acc, (i, avg)| {
                    let i = i as u128;
                    ((acc * i) + avg) / (i + 1)
                });
            Duration::from_nanos(avg_latency.try_into().unwrap())
        },
    )
}

fn bench_mutex<M>(c: &mut Criterion, ctx: BenchContext)
where
    M: Mutex<BenchValue> + Send + Sync + 'static,
{
    let bench_type_name = match ctx.bench_type {
        BenchType::Latency => "latency",
        BenchType::Throughput => "throughput",
    };

    c.bench_function(
        &format!(
            "[sync_mutex_{}] {} threads={} work_per_lock={}",
            bench_type_name,
            M::NAME,
            ctx.num_threads,
            ctx.work_per_critical_section,
        ),
        |b| {
            b.iter_custom(|iters| match ctx.bench_type {
                BenchType::Throughput => bench_throughput::<M>(ctx, iters),
                BenchType::Latency => bench_latency::<M>(ctx, iters),
            })
        },
    );
}

fn bench_all(c: &mut Criterion, ctx: BenchContext) {
    bench_mutex::<std::sync::Mutex<BenchValue>>(c, ctx);
    bench_mutex::<parking_lot::Mutex<BenchValue>>(c, ctx);
    bench_mutex::<yaar_sync_lock::Mutex<BenchValue>>(c, ctx);
    bench_mutex::<yaar_lock::sync::OsMutex<BenchValue>>(c, ctx);

    bench_mutex::<std_lock::Mutex<BenchValue>>(c, ctx);
    bench_mutex::<spin_lock::Mutex<BenchValue>>(c, ctx);
    bench_mutex::<os_lock::Mutex<BenchValue>>(c, ctx);

    #[cfg(windows)]
    bench_mutex::<nt_lock::Mutex<BenchValue>>(c, ctx);
    #[cfg(unix)]
    bench_mutex::<yield_lock::Mutex<BenchValue>>(c, ctx);
}

fn bench_threads(c: &mut Criterion, bench_type: BenchType, work_per_critical_section: usize) {
    let max_threads = num_cpus::get();
    let mut num_threads = Vec::new();
    num_threads.push(max_threads * 2);
    num_threads.push(max_threads);

    let mut stop = false;
    let mut rest = max_threads / 2;
    while !stop && rest < max_threads {
        if rest == 1 {
            if !num_threads.contains(&2) {
                num_threads.push(2);
            }
            stop = true;
        }
        num_threads.push(rest);
        rest /= 2;
    }

    for &num_threads in num_threads.iter().rev() {
        bench_all(
            c,
            BenchContext {
                bench_type,
                num_threads,
                work_per_critical_section,
            },
        );
    }
}

fn lock_latency_short(c: &mut Criterion) {
    bench_threads(c, BenchType::Latency, 1);
}

fn lock_latency_long(c: &mut Criterion) {
    bench_threads(c, BenchType::Latency, 20);
}

fn lock_throughput_short(c: &mut Criterion) {
    bench_threads(c, BenchType::Throughput, 1);
}

fn lock_throughput_long(c: &mut Criterion) {
    bench_threads(c, BenchType::Throughput, 20);
}

const NOISE_PERCENT: f64 = 0.10;
const WARM_UP: Duration = Duration::from_millis(500);
const MEASURE: Duration = Duration::from_millis(500);

criterion_group! {
    name = throughput_short;
    config = Criterion::default()
        .noise_threshold(NOISE_PERCENT)
        .warm_up_time(WARM_UP)
        .measurement_time(MEASURE);
    targets = lock_throughput_short,
}

criterion_group! {
    name = throughput_long;
    config = Criterion::default()
        .noise_threshold(NOISE_PERCENT)
        .warm_up_time(WARM_UP)
        .measurement_time(MEASURE);
    targets = lock_throughput_long,
}

criterion_group! {
    name = latency_short;
    config = Criterion::default()
        .noise_threshold(NOISE_PERCENT)
        .warm_up_time(WARM_UP)
        .measurement_time(MEASURE);
    targets = lock_latency_short,
}

criterion_group! {
    name = latency_long;
    config = Criterion::default()
        .noise_threshold(NOISE_PERCENT)
        .warm_up_time(WARM_UP)
        .measurement_time(MEASURE);
    targets = lock_latency_long,
}

criterion_main!(
    throughput_short,
    throughput_long,
    latency_short,
    latency_long
);
