#[macro_use]
extern crate criterion;

#[path = "prelude.rs"]
mod prelude;
use prelude::*;

use criterion::{Bencher, Criterion};
use std::{mem::drop, sync::Arc};

fn bench_yield_many<E: BenchExecutor>(b: &mut Bencher) {
    const NUM_YIELDS: usize = 10_000;
    const NUM_TASKS: usize = 100;

    b.iter_custom(|iters| {
        E::record(iters, |wg| async move {
            wg.reset(NUM_TASKS);

            for _ in 0..NUM_TASKS {
                let wg = wg.clone();
                E::spawn(async move {
                    for _ in 0..NUM_YIELDS {
                        tokio::task::yield_now().await;
                    }
                    wg.done();
                });
            }
        })
    })
}

fn bench_spawn_many<E: BenchExecutor>(b: &mut Bencher) {
    const NUM_TASKS: usize = 100_000;

    b.iter_custom(|iters| {
        E::record(iters, |wg| async move {
            wg.reset(NUM_TASKS);

            for _ in 0..NUM_TASKS {
                let wg = wg.clone();
                E::spawn(async move {
                    wg.done();
                });
            }
        })
    })
}

fn bench_chained_spawn<E: BenchExecutor>(b: &mut Bencher) {
    const SPAWN_DEPTH: usize = 2_000;

    b.iter_custom(|iters| {
        E::record(iters, |wg| async move {
            fn recurse<E: BenchExecutor>(wg: Arc<WaitGroup>, n: usize) {
                match n {
                    0 => wg.done(),
                    _ => drop(E::spawn(async move {
                        recurse::<E>(wg, n - 1);
                    })),
                }
            }

            wg.reset(1);
            E::spawn(async move {
                recurse::<E>(wg, SPAWN_DEPTH);
            });
        })
    })
}

fn yield_many(c: &mut Criterion) {
    c.bench_function("tokio yield_many", |b| bench_yield_many::<TokioExecutor>(b));
    c.bench_function("yaar yield_many", |b| bench_yield_many::<YaarExecutor>(b));
}

fn spawn_many(c: &mut Criterion) {
    c.bench_function("tokio spawn_many", |b| bench_spawn_many::<TokioExecutor>(b));
    c.bench_function("yaar spawn_many", |b| bench_spawn_many::<YaarExecutor>(b));
}

fn chained_spawn(c: &mut Criterion) {
    c.bench_function("tokio chained_spawn", |b| {
        bench_chained_spawn::<TokioExecutor>(b)
    });
    c.bench_function("yaar chained_spawn", |b| {
        bench_chained_spawn::<YaarExecutor>(b)
    });
}

criterion_group!(bench_executors, yield_many, spawn_many, chained_spawn);
criterion_main!(bench_executors);
