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

fn yield_now(c: &mut Criterion) {
    let tasks = 100_000;
    c.bench_function("tokio yield_now", |b| {
        bench_yield::<TokioExecutor>(b, tasks)
    });
    c.bench_function("yaar yield_now", |b| bench_yield::<YaarExecutor>(b, tasks));
}

criterion_group!(bench_executors, yield_now,);

criterion_main!(bench_executors);
