use pin_project_lite::pin_project;
use std::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::sync::Notify;

#[derive(Default)]
pub struct WaitGroup {
    notify: Notify,
    count: AtomicUsize,
}

impl WaitGroup {
    pub fn reset(&self, count: usize) {
        self.count.store(count, Ordering::Relaxed);
    }

    pub fn done(&self) {
        if self.count.fetch_sub(1, Ordering::Release) == 1 {
            self.notify.notify_one();
        }
    }

    pub async fn ready(&self) {
        while self.count.load(Ordering::Acquire) != 0 {
            self.notify.notified().await;
        }
    }
}

pub trait BenchExecutor {
    type JoinHandle: Future<Output = ()> + Send + 'static;

    fn spawn<F: Future<Output = ()> + Send + 'static>(future: F) -> Self::JoinHandle;

    fn block_on<F: Future<Output = ()>>(future: F);

    fn record<F: Future<Output = ()>>(
        iters: u64,
        mut f: impl FnMut(Arc<WaitGroup>) -> F,
    ) -> Duration {
        let mut elapsed = None;
        Self::block_on(async {
            let wg = Arc::new(WaitGroup::default());
            let started = Instant::now();
            for _ in 0..iters {
                f(wg.clone()).await;
                wg.ready().await;
            }
            elapsed = Some(started.elapsed());
        });
        elapsed.unwrap()
    }
}

pub struct TokioExecutor;

pin_project! {
    pub struct TokioJoinHandle<T> {
        #[pin]
        handle: tokio::task::JoinHandle<T>
    }
}

impl<T> Future for TokioJoinHandle<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project().handle.poll(ctx) {
            Poll::Ready(result) => Poll::Ready(result.unwrap()),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl BenchExecutor for TokioExecutor {
    type JoinHandle = TokioJoinHandle<()>;

    fn spawn<F: Future<Output = ()> + Send + 'static>(future: F) -> Self::JoinHandle {
        TokioJoinHandle {
            handle: tokio::spawn(future),
        }
    }

    fn block_on<F: Future<Output = ()>>(future: F) {
        tokio::runtime::Builder::new_multi_thread()
            .build()
            .unwrap()
            .block_on(future)
    }
}

pub struct YaarExecutor;

impl BenchExecutor for YaarExecutor {
    type JoinHandle = yaar::task::JoinHandle<()>;

    fn spawn<F: Future<Output = ()> + Send + 'static>(future: F) -> Self::JoinHandle {
        yaar::task::spawn(future)
    }

    fn block_on<F: Future<Output = ()>>(future: F) {
        yaar::task::block_on(future)
    }
}
