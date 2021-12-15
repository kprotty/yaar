#[macro_use]
extern crate criterion;

#[path = "prelude.rs"]
mod prelude;
use prelude::*;

use criterion::{black_box, Bencher, Criterion};

fn bench_yield<E: BenchExecutor>(b: &mut Bencher, tasks: usize) {
    b.iter_custom(|iters| {
        E::record(async move {
            let handles = (0..tasks).map(|_| {
                E::spawn(async move {
                    for _ in 0..black_box(iters * 10) {
                        tokio::task::yield_now().await;
                    }
                })
            });

            for handle in handles.collect::<Vec<_>>() {
                handle.await;
            }
        })
    })
}

fn bench_spawn<E: BenchExecutor>(b: &mut Bencher, tasks: usize, concurrency: usize) {
    b.iter_custom(|iters| {
        E::record(async move {
            let handles = (0..concurrency).map(|_| {
                E::spawn(async move {
                    let handles = (0..(tasks / concurrency)).map(|_| {
                        E::spawn(async move {
                            for _ in 0..black_box(iters) {
                                std::hint::spin_loop();
                            }
                        })
                    });

                    for handle in handles.collect::<Vec<_>>() {
                        handle.await;
                    }
                })
            });

            for handle in handles.collect::<Vec<_>>() {
                handle.await;
            }
        })
    })
}

fn yield_now(c: &mut Criterion) {
    let num_tasks = 100_000;
    c.bench_function("tokio yield_now", |b| {
        bench_yield::<TokioExecutor>(b, num_tasks)
    });
    c.bench_function("yaar yield_now", |b| {
        bench_yield::<YaarExecutor>(b, num_tasks)
    });
}

fn spawn_spmc(c: &mut Criterion) {
    let concurrency = 1;
    let num_tasks = 1_000_000;
    c.bench_function("tokio spawn (spmc)", |b| {
        bench_spawn::<TokioExecutor>(b, num_tasks, concurrency)
    });
    c.bench_function("yaar spawn (spmc)", |b| {
        bench_spawn::<YaarExecutor>(b, num_tasks, concurrency)
    });
}

fn spawn_mpmc(c: &mut Criterion) {
    let concurrency = num_cpus::get();
    let num_tasks = 1_000_000;
    c.bench_function("tokio spawn (spmc)", |b| {
        bench_spawn::<TokioExecutor>(b, num_tasks, concurrency)
    });
    c.bench_function("yaar spawn (spmc)", |b| {
        bench_spawn::<YaarExecutor>(b, num_tasks, concurrency)
    });
}

criterion_group!(bench_executors, yield_now, spawn_spmc, spawn_mpmc);

criterion_main!(bench_executors);
