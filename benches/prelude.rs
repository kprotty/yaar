use pin_project_lite::pin_project;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};

pub trait BenchExecutor {
    type JoinHandle: Future<Output = ()>;

    fn spawn<F: Future<Output = ()> + Send + 'static>(future: F) -> Self::JoinHandle;

    fn block_on<F: Future<Output = ()>>(future: F);

    fn record<F: Future<Output = ()>>(future: F) -> Duration {
        let mut elapsed = None;
        Self::block_on(async {
            let started = Instant::now();
            future.await;
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
    type JoinHandle = yaar::JoinHandle<()>;

    fn spawn<F: Future<Output = ()> + Send + 'static>(future: F) -> Self::JoinHandle {
        yaar::spawn(future)
    }

    fn block_on<F: Future<Output = ()>>(future: F) {
        yaar::block_on(future)
    }
}
