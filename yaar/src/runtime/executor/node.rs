//! Numa-Aware, Multithreaded Scheduler based on
//! https://docs.google.com/document/u/0/d/1d3iI2QWURgDIsSR6G2275vMeQ_X7w-qxM2Vp7iGwwuM/pub.

use super::{
    super::{
        platform::Platform,
        task::{GlobalQueue, LocalQueue, Task},
    },
    with_executor_as, Executor,
};
use crate::util::CachePadded;
use core::{
    cell::Cell,
    future::Future,
    num::NonZeroUsize,
    ptr::NonNull,
    slice::from_raw_parts,
    sync::atomic::{AtomicUsize, Ordering},
};
use yaar_lock::sync::{RawMutex, WordLock};

pub fn run_using<T, P: Platform>(
    platform: &P,
    start_node: usize,
    nodes: &[NonNull<Node<P>>],
    future: impl Future<Output = T>,
) -> T {
    if nodes.len() == 0 {
        panic!("Empty `nodes` (serial execution) isn't currently supported");
    }

    let start_node = nodes[start_node.min(nodes.len() - 1)]
}

pub struct Node<P: Platform> {
    platform: *const P,
    cpu_affinity: P::CpuAffinity,
    worker_ptr: *const Worker,
    worker_len: usize,
    workers_idle: AtomicUsize,
    workers_searching: AtomicUsize,
    run_queue: GlobalQueue<WordLock<P::ThreadEvent>>,
    pool: RawMutex<CachePadded<WorkerPool>, P::ThreadEvent>,
}

unsafe impl<P: Platform> Sync for Node<P> {}

impl<P: Platform> Node<P> {
    pub fn new(
        platform: &P,
        workers: &[Worker],
        max_threads: NonZeroUsize,
        cpu_affinity: P::CpuAffinity,
    ) -> Self {
        Self {
            platform,
            cpu_affinity,
            worker_ptr: workers.as_ptr(),
            worker_len: workers.len(),
            workers_idle: AtomicUsize::new(0),
            workers_searching: AtomicUsize::new(0),
            run_queue: GlobalQueue::default(),
            pool: RawMutex::new(CachePadded::new(WorkerPool::new(max_threads))),
        }
    }

    #[inline]
    pub fn workers(&self) -> &[Worker] {
        unsafe { from_raw_parts(self.worker_ptr, self.worker_len) }
    }
}

struct WorkerPool {
    max_threads: usize,
    free_threads: usize,
    idle_threads: Option<NonNull<Thread>>,
    idle_workers: Option<NonNull<Worker>>,
}

impl WorkerPool {
    pub fn new(max_threads: NonZeroUsize) -> Self {
        Self {
            max_threads: max_threads.get(),
            free_threads: max_threads.get(),
            idle_threads: None,
            idle_workers: None,
        }
    }

    pub fn put_worker<P: Platform>(&mut self, node: &Node<P>, worker: &Worker) {
        let workers_idle = node.workers_idle.load(Ordering::Relaxed);
        assert!(workers_idle <= node.workers().len());
        worker.next.set(self.idle_workers);
        self.idle_workers = NonNull::new(worker as *const _ as *mut _);
        node.workers_idle.store(workers_idle + 1, Ordering::Release);
    }
}

struct Thread {
    next: Option<NonNull<Self>>,
}

#[derive(Default)]
pub struct Worker {
    next: Cell<Option<NonNull<Self>>>,
    node: usize,
    run_queue: LocalQueue,
}
