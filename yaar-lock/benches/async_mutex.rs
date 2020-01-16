use async_std::{sync::Mutex as AsyncStdMutex, task};
use criterion::{criterion_group, criterion_main, Benchmark, Criterion};
use futures_intrusive::sync::{Mutex as IntrusiveMutex, Semaphore};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::Mutex as TokioMutex;
use yaar_lock::futures::Mutex as YaarMutex;

const TEST_SECS: u64 = 20;
const ITERATIONS: usize = 50;
const NUM_YIELDS: usize = 10;
const YIELD_CHANCE: usize = 25;

trait Block {
    fn block_on<F: Future<Output = ()>>(&self, f: F);
}

struct FakeAsyncStdRuntime;

impl Block for FakeAsyncStdRuntime {
    fn block_on<F: Future<Output = ()>>(&self, f: F) {
        task::block_on(f);
    }
}

struct Yield {
    iter: usize,
}

impl Future for Yield {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<()> {
        if self.iter == 0 {
            Poll::Ready(())
        } else {
            self.iter -= 1;
            ctx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

macro_rules! run_mutex {
    ($b:ident, $setup:expr, $spawn:expr, $new:expr, $iters:expr, $threads:expr, $drop:expr) => {
        #[allow(unused_mut)]
        let mut rt = $setup;
        $b.iter(|| {
            rt.block_on(async {
                let mutex = Arc::new($new);
                let mut tasks = Vec::new();
                let sema = Arc::new(Semaphore::new(false, 0));

                let num_tasks = $threads;
                for _ in 0..num_tasks {
                    let mutex = mutex.clone();
                    let sema = sema.clone();
                    tasks.push($spawn(async move {
                        for count in 0..$iters {
                            let guard = mutex.lock().await;
                            if YIELD_CHANCE != 0 && (count % (100 / YIELD_CHANCE) == 0) {
                                Yield { iter: NUM_YIELDS }.await;
                            }
                            $drop(guard);
                        }
                        sema.release(1);
                    }));
                }

                sema.acquire(num_tasks).await;
            })
        })
    };
}

macro_rules! benchmarks {
    ($c:ident, $setup:expr, $spawn:expr, $name:literal, $new:expr, $drop:expr,) => {
        $c.bench(
            $name,
            Benchmark::new("contention", |b| {
                run_mutex!(b, $setup, $spawn, $new, ITERATIONS, num_cpus::get(), $drop);
            })
            .with_function("no_contention", |b| {
                run_mutex!(b, $setup, $spawn, $new, ITERATIONS, 1, $drop);
            }),
        );
    };
}

fn tokio_rt_intrusive_fair(c: &mut Criterion) {
    benchmarks!(
        c,
        tokio::runtime::Runtime::new().unwrap(),
        tokio::spawn,
        "tokio/futures_intrusive(fair)",
        IntrusiveMutex::new((), true),
        |m| std::mem::drop(m),
    );
}

fn tokio_rt_intrusive_unfair(c: &mut Criterion) {
    benchmarks!(
        c,
        tokio::runtime::Runtime::new().unwrap(),
        tokio::spawn,
        "tokio/futures_intrusive(unfair)",
        IntrusiveMutex::new((), false),
        |m| std::mem::drop(m),
    );
}

fn tokio_rt_yaar_fair(c: &mut Criterion) {
    benchmarks!(
        c,
        tokio::runtime::Runtime::new().unwrap(),
        tokio::spawn,
        "tokio/yaar(fair)",
        YaarMutex::new(()),
        |m: yaar_lock::futures::MutexGuard<'_, ()>| m.unlock_fair(),
    );
}

fn tokio_rt_yaar_unfair(c: &mut Criterion) {
    benchmarks!(
        c,
        tokio::runtime::Runtime::new().unwrap(),
        tokio::spawn,
        "tokio/yaar(unfair)",
        YaarMutex::new(()),
        |m| std::mem::drop(m),
    );
}

fn tokio_rt_async_std(c: &mut Criterion) {
    benchmarks!(
        c,
        tokio::runtime::Runtime::new().unwrap(),
        tokio::spawn,
        "tokio/async_std",
        AsyncStdMutex::new(()),
        |m| std::mem::drop(m),
    );
}

fn tokio_rt_tokio(c: &mut Criterion) {
    benchmarks!(
        c,
        tokio::runtime::Runtime::new().unwrap(),
        tokio::spawn,
        "tokio/tokio",
        TokioMutex::new(()),
        |m| std::mem::drop(m),
    );
}

fn async_std_intrusive_fair(c: &mut Criterion) {
    benchmarks!(
        c,
        FakeAsyncStdRuntime {},
        task::spawn,
        "async_std/futures_intrusive(fair)",
        IntrusiveMutex::new((), true),
        |m| std::mem::drop(m),
    );
}

fn async_std_intrusive_unfair(c: &mut Criterion) {
    benchmarks!(
        c,
        FakeAsyncStdRuntime {},
        task::spawn,
        "async_std/futures_intrusive(unfair)",
        IntrusiveMutex::new((), false),
        |m| std::mem::drop(m),
    );
}

fn async_std_yaar_fair(c: &mut Criterion) {
    benchmarks!(
        c,
        FakeAsyncStdRuntime {},
        task::spawn,
        "async_std/yaar(fair)",
        YaarMutex::new(()),
        |m: yaar_lock::futures::MutexGuard<'_, ()>| m.unlock_fair(),
    );
}

fn async_std_yaar_unfair(c: &mut Criterion) {
    benchmarks!(
        c,
        FakeAsyncStdRuntime {},
        task::spawn,
        "async_std/yaar(unfair)",
        YaarMutex::new(()),
        |m| std::mem::drop(m),
    );
}

fn async_std_async_std(c: &mut Criterion) {
    benchmarks!(
        c,
        FakeAsyncStdRuntime {},
        task::spawn,
        "async_std/async_std",
        AsyncStdMutex::new(()),
        |m| std::mem::drop(m),
    );
}

fn async_std_tokio(c: &mut Criterion) {
    benchmarks!(
        c,
        FakeAsyncStdRuntime {},
        task::spawn,
        "async_std/tokio",
        TokioMutex::new(()),
        |m| std::mem::drop(m),
    );
}

criterion_group! {
    name = benches;
    config = Criterion::default().measurement_time(Duration::from_secs(TEST_SECS));
    targets =
        // tokio
        tokio_rt_intrusive_fair,
        tokio_rt_intrusive_unfair,
        tokio_rt_yaar_fair,
        tokio_rt_yaar_unfair,
        tokio_rt_async_std,
        tokio_rt_tokio,
        // async-std
        async_std_intrusive_fair,
        async_std_intrusive_unfair,
        async_std_yaar_fair,
        async_std_yaar_unfair,
        async_std_async_std,
        async_std_tokio
}

criterion_main!(benches);
