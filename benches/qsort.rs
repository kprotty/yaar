#[macro_use]
extern crate criterion;

use criterion::{black_box, Bencher, Criterion};
use std::{
    future::Future,
    slice::from_raw_parts,
    time::{Duration, Instant},
};

fn shuffle(arr: &mut [i32]) {
    let mut xs: u32 = 0xdeadbeef;
    for i in 0..arr.len() {
        xs ^= xs << 13;
        xs ^= xs >> 17;
        xs ^= xs << 5;
        let j = (xs as usize) % (i + 1);
        arr.swap(i, j);
    }
}

fn setup<F>(size: usize, with_array: impl FnOnce(Box<[i32]>) -> F) -> F {
    let mut arr = (0..size)
        .map(|i| i.try_into().unwrap())
        .collect::<Vec<i32>>()
        .into_boxed_slice();

    shuffle(&mut arr);
    with_array(black_box(arr))
}

fn verify(arr: &[i32]) -> bool {
    black_box(arr.windows(2).all(|i| i[0] <= i[1]))
}

fn run_qsort_sync(size: usize, qsort_fn: impl FnOnce(&mut [i32])) -> Duration {
    setup(size, |arr| {
        let start = Instant::now();
        qsort_fn(&mut arr);
        let elapsed = start.elapsed();

        assert!(verify(&arr));
        elapsed
    })
}

async fn run_qsort_async<Q, F>(size: usize, qsort_fn: Q) -> Duration
where
    Q: FnOnce(&'static mut [i32]) -> F,
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    setup(size, |arr| async move {
        let arr_ptr = arr.as_mut_ptr() as usize;
        let arr_len = arr.len();

        let start = Instant::now();
        qsort_fn(arr).await;
        let elapsed = start.elapsed();

        let arr = unsafe { from_raw_parts(arr_ptr as *const i32, arr_len) };
        assert!(verify(arr));
        elapsed
    })
    .await
}

fn insertion_sort(arr: &mut [i32]) {
    for i in 1..arr.len() {
        let mut n = i;
        while n > 0 && arr[n] < arr[n - 1] {
            arr.swap(n, n - 1);
            n -= 1;
        }
    }
}

fn partition(arr: &mut [i32]) -> usize {
    let pivot = arr.len() - 1;
    let mut i = 0;
    for j in 0..pivot {
        if arr[j] <= arr[pivot] {
            arr.swap(i, j);
            i += 1;
        }
    }
    arr.swap(i, pivot);
    i
}

fn quick_sort_sync(arr: &mut [i32], sync_fn: impl FnOnce(&mut [i32], &mut [i32])) {
    if arr.len() <= 32 {
        insertion_sort(arr);
    } else {
        let mid = partition(arr);
        let (low, high) = arr.split_at_mut(mid);
        sync_fn(low, high);
    }
}

async fn quick_sort_async<S, F>(arr: &'static mut [i32], sync_fn: S)
where
    S: FnOnce(&'static mut [i32], &'static mut [i32]) -> F,
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    if arr.len() <= 32 {
        insertion_sort(arr);
    } else {
        let mid = partition(arr);
        let (low, high) = arr.split_at_mut(mid);
        sync_fn(low, high).await;
    }
}

fn qsort_sync(b: &mut Criterion) {
    b.bench_function("rayon-join", |b| {
        fn qsort_rayon_join(arr: &mut [i32]) {
            quick_sort_sync(arr, |low, high| {
                rayon::join(|| qsort_rayon_join(low), || qsort_rayon_join(high));
            });
        }

        b.iter_custom(|iters| {
            let size = iters * 10_000;
            run_qsort_sync(size, qsort_rayon_join);
        })
    });

    b.bench_function("rayon-scope", |b| {
        fn qsort_rayon_scope(arr: &mut [i32]) {
            quick_sort_sync(arr, |low, high| {
                rayon::scope(|s| {
                    s.spawn(|_| qsort_rayon_scope(low));
                    s.spawn(|_| qsort_rayon_scope(high));
                });
            });
        }

        b.iter_custom(|iters| {
            let size = iters * 10_000;
            run_qsort_sync(size, qsort_rayon_join);
        })
    });
}

fn qsort_async(b: &mut Criterion) {
    // TODO: tokio + yaar
}

criterion_group!(qsort, qsort_sync, qsort_async,);
criterion_main!(qsort);
