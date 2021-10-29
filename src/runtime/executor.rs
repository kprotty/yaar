use super::{queue::Queue, task::TaskRunnable, thread::Thread};
use std::{
    sync::atomic::{fence, AtomicUsize, Ordering},
    sync::Arc,
};

pub struct Worker {
    idle_next: AtomicUsize,
    run_queue: Queue,
}

pub struct Executor {
    injected: Queue,
    workers: Box<[Worker]>,
    idle: AtomicUsize,
    searching: AtomicUsize,
}

impl Executor {
    pub fn with_worker<F>(f: impl FnOnce(&Arc<Self>, usize) -> F) -> Option<F> {
        Thread::with_current(|thread| {
            let worker_index = thread.worker_index.expect("Using thread without a worker");
            f(&thread.executor, worker_index)
        })
    }

    pub fn schedule(self: &Arc<Self>, task: Arc<dyn TaskRunnable>, worker_index: Option<usize>) {
        match worker_index {
            Some(worker_index) => self.workers[worker_index].run_queue.push(task),
            None => {
                self.injected.push(task);
                fence(Ordering::SeqCst);
            }
        }
        self.notify()
    }

    fn notify(self: &Arc<Self>) {
        if self.peek_idle_worker().is_none() {
            return;
        }

        let searching = self.searching.load(Ordering::Relaxed);
        debug_assert!(searching <= self.workers.len());
        if searching > 0 {
            return;
        }

        if let Err(searching) =
            self.searching
                .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
        {
            debug_assert!(searching <= self.workers.len());
            return;
        }

        if let Some(worker_index) = self.pop_idle_worker() {
            unimplemented!("TODO: try_spawn into pool")
        }

        let searching = self.searching.fetch_sub(1, Ordering::Relaxed);
        assert!(searching > 0);
    }

    fn shutdown(&self) {}
}

#[derive(Copy, Clone, Eq, PartialEq)]
struct Idle {
    top_index: Option<usize>,
    aba: usize,
}

impl Idle {
    const BITS: u32 = usize::BITS / 2;
    const MASK: usize = (1 << Self::BITS) - 1;
}

impl Into<usize> for Idle {
    fn into(self) -> usize {
        let top_index = self.top_index.map(|i| i + 1).unwrap_or(0);
        assert!(top_index <= Self::MASK);

        let aba = self.aba & Self::MASK;
        (aba << Self::BITS) | top_index
    }
}

impl From<usize> for Idle {
    fn from(value: usize) -> Self {
        Self {
            top_index: match value & Self::MASK {
                0 => None,
                index => Some(index - 1),
            },
            aba: value >> Self::BITS,
        }
    }
}

impl Executor {
    fn push_idle_worker(&self, worker_index: usize) {
        let _ = self
            .idle
            .fetch_update(Ordering::Release, Ordering::Relaxed, |idle| {
                let mut idle = Idle::from(idle);
                self.workers[worker_index]
                    .idle_next
                    .store(idle.into(), Ordering::Relaxed);

                idle.top_index = Some(worker_index);
                idle.aba += 1;
                Some(idle.into())
            });
    }

    fn peek_idle_worker(&self) -> Option<usize> {
        let idle = self.idle.load(Ordering::Acquire);
        Idle::from(idle).top_index
    }

    fn pop_idle_worker(&self) -> Option<usize> {
        self.idle
            .fetch_update(Ordering::Acquire, Ordering::Acquire, |idle| {
                let mut idle = Idle::from(idle);
                let worker_index = idle.top_index?;

                let next = self.workers[worker_index].idle_next.load(Ordering::Relaxed);
                idle.top_index = Idle::from(next).top_index;
                Some(idle.into())
            })
            .ok()
            .and_then(|idle| Idle::from(idle).top_index)
    }
}
