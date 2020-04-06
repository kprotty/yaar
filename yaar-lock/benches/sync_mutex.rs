#[macro_use]
extern crate criterion;

use criterion::Criterion;
use yaar_lock::utils::CachePadded;
use std::{
    thread,
    mem::drop,
    time::{Instant, Duration},
    sync::{Arc, Barrier},
};

mod sync_mutexes;
use sync_mutexes::*;

fn bench_all(c: &mut Criterion, ctx: BenchContext) {
    bench_mutex::<std::sync::Mutex<f64>>(c, ctx);
    bench_mutex::<parking_lot::Mutex<f64>>(c, ctx);
    bench_mutex::<yaar_lock::sync::Mutex<f64>>(c, ctx);
    
    bench_mutex::<std_lock::Mutex<f64>>(c, ctx);
    bench_mutex::<spin_lock::Mutex<f64>>(c, ctx);
    #[cfg(windows)] bench_mutex::<nt_lock::Mutex<f64>>(c, ctx);
}

#[derive(Copy, Clone)]
struct BenchContext {
    num_threads: usize,
    work_per_critical_section: usize,
}

fn bench_mutex<M: Mutex<f64> + Send + Sync + 'static>(
    c: &mut Criterion,
    ctx: BenchContext,
) {
    c.bench_function(
        &format!(
            "[sync_mutex] {} threads={}", 
            M::NAME,
            ctx.num_threads,
        ),
        |b| b.iter_custom(|iters| {
            let mutex = Arc::new(CachePadded::new(M::new(0.0)));
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
                                    *shared_value += local_value;
                                    *shared_value *= 1.01;
                                    local_value = *shared_value;
                                }
                            });
                        }
                        local_value
                    })
                })
                .collect::<Vec<_>>();

            let start = Instant::now();
            barrier.wait();
            threads.into_iter().map(|t| t.join().unwrap()).for_each(drop);
            start.elapsed()
        })
    );
}

fn bench_throughput(c: &mut Criterion, work_per_critical_section: usize) {
    let max_threads = num_cpus::get();

    let mut last_tested = 0;
    let mut num_threads = 1;
    while num_threads < max_threads / 2 {
        last_tested = num_threads;
        bench_all(c, BenchContext {
            num_threads,
            work_per_critical_section,
        });
        if num_threads < 2 {
            num_threads += 1;
        } else {
            num_threads *= 2;
        }
    }

    if last_tested < max_threads / 2 {
        bench_all(c, BenchContext {
            num_threads: max_threads / 2,
            work_per_critical_section,
        });
    }

    bench_all(c, BenchContext {
        num_threads: max_threads,
        work_per_critical_section,
    });
    bench_all(c, BenchContext {
        num_threads: max_threads * 2,
        work_per_critical_section,
    });
}

fn no_critical_section(c: &mut Criterion) {
    bench_throughput(c, 0);
}

#[allow(unused)]    
fn small_critical_section(c: &mut Criterion) {
    bench_throughput(c, 2);
}

#[allow(unused)]
fn large_critical_section(c: &mut Criterion) {
    bench_throughput(c, 5);
}

criterion_group! {
    name = sync_mutex;
    config = Criterion::default()
        .noise_threshold(0.10)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(1));
    targets = 
        no_critical_section,
        // small_critical_section,
        // large_critical_section,
}

criterion_main!(sync_mutex);