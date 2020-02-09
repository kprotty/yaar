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
use yaar_lock::future::Mutex as YaarMutex;

const TEST_SECS: u64 = 10;
const ITERATIONS: usize = 50;
const NUM_YIELDS: usize = 10;
const YIELD_CHANCE: usize = 25;

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
    ($b:ident, $setup:expr, $spawn:expr, $new:expr, $iters:expr, $threads:expr,) => {
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
                            let _guard = mutex.lock().await;
                            if YIELD_CHANCE != 0 && (count % (100 / YIELD_CHANCE) == 0) {
                                Yield { iter: NUM_YIELDS }.await;
                            }
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
    ($c:ident, $setup:expr, $spawn:expr, $name:literal, $new:expr,) => {
        $c.bench(
            $name,
            Benchmark::new("contention", |b| {
                run_mutex!(b, $setup, $spawn, $new, ITERATIONS, num_cpus::get(),);
            })
            .with_function("no_contention", |b| {
                run_mutex!(b, $setup, $spawn, $new, ITERATIONS, 1,);
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
    );
}

fn tokio_rt_intrusive_unfair(c: &mut Criterion) {
    benchmarks!(
        c,
        tokio::runtime::Runtime::new().unwrap(),
        tokio::spawn,
        "tokio/futures_intrusive(unfair)",
        IntrusiveMutex::new((), false),
    );
}

fn tokio_rt_yaar_fair(c: &mut Criterion) {
    benchmarks!(
        c,
        tokio::runtime::Runtime::new().unwrap(),
        tokio::spawn,
        "tokio/yaar(fair)",
        YaarMutex::new((), true),
    );
}

fn tokio_rt_yaar_unfair(c: &mut Criterion) {
    benchmarks!(
        c,
        tokio::runtime::Runtime::new().unwrap(),
        tokio::spawn,
        "tokio/yaar(unfair)",
        YaarMutex::new((), false),
    );
}

fn tokio_rt_tokio(c: &mut Criterion) {
    benchmarks!(
        c,
        tokio::runtime::Runtime::new().unwrap(),
        tokio::spawn,
        "tokio/tokio",
        TokioMutex::new(()),
    );
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(20)
        .measurement_time(Duration::from_secs(TEST_SECS));
    targets =
        tokio_rt_yaar_unfair,
        tokio_rt_intrusive_unfair,
        tokio_rt_yaar_fair,
        tokio_rt_intrusive_fair,
        tokio_rt_tokio,
}

criterion_main!(benches);
