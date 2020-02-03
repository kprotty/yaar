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
    marker::PhantomData,
    slice::from_raw_parts,
    sync::atomic::{AtomicUsize, Ordering},
};
use yaar_lock::sync::{RawMutex, WordLock};

pub enum RunError {
    NoNode,
    NoWorker,
}

pub fn run_using<T, P: Platform + 'static>(
    platform: &P,
    nodes: &[NonNull<Node<P>>],
    future: impl Future<Output = T>,
) -> Result<T, RunError> {
    let nodes: &[&Node<P>] = unsafe { from_raw_parts(nodes.as_ptr() as *const _, nodes.len()) };
    if nodes.len() == 0 {
        return Err(RunError::NoNode);
    }

    let executor = NodeExecutor {
        pending_tasks: AtomicUsize::new(0),
        platform: NonNull::new(platform as *const P as *mut P).unwrap(),
        nodes_ptr: &nodes,
    };

    for node in nodes {

    }
    
    with_executor_as(&executor, |_| {
        let node = nodes[0];
        let mut pool = node.pool.lock();
        pool.free_threads -= 1;
        Thread::run(match pool.find_worker() {
            Some(worker) => worker as usize,
            None => return Err(RunError::NoWorker),
        });
        
    })
}

struct NodeExecutor<P: Platform> {
    pending_tasks: AtomicUsize,
    platform: NonNull<P>,
    nodes_ptr: usize,
}

unsafe impl<P: Platform> Sync for NodeExecutor<P> {}

impl<P: Platform> NodeExecutor<P> {
    fn platform(&self) -> &P {
        unsafe { self.platform.as_ref() }
    }

    fn nodes(&self) -> &[&Node<P>] {
        unsafe { &*(self.nodes_ptr as *const _) }
    }
}

impl<P: Platform> Executor for NodeExecutor<P> {
    fn schedule(&self, task: &Task) {
        // TODO
    }
}

pub struct Node<P: Platform> {
    executor: Cell<Option<NonNull<NodeExecutor<P>>>>,
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
        workers: &[Worker],
        max_threads: NonZeroUsize,
        cpu_affinity: P::CpuAffinity,
    ) -> Self {
        Self {
            executor: Cell::new(None),
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

    #[inline]
    pub fn executor(&self) -> &NodeExecutor<P> {
        unsafe { &*self.executor.get().expect("No executor is set").as_ptr() }
    }
}

struct WorkerPool<P: Platform> {
    phantom: PhantomData<P>,
    max_threads: usize,
    free_threads: usize,
    idle_threads: Option<NonNull<Thread>>,
    idle_workers: Option<NonNull<Worker>>,
}

impl<P: Platform> WorkerPool<P> {
    pub fn new(max_threads: NonZeroUsize) -> Self {
        Self {
            phantom: PhantomData,
            max_threads: max_threads.get(),
            free_threads: max_threads.get(),
            idle_threads: None,
            idle_workers: None,
        }
    }

    pub fn put_worker(&mut self, node: &Node<P>, worker: &Worker) {
        let workers_idle = node.workers_idle.load(Ordering::Relaxed);
        assert!(workers_idle <= node.workers().len());
        worker.next.set(self.idle_workers);
        self.idle_workers = NonNull::new(worker as *const _ as *mut _);
        node.workers_idle.store(workers_idle + 1, Ordering::Release);
    }

    pub fn find_worker(&mut self, node: &Node<P>) -> Option<&Worker> {
        self.idle_workers.map(|worker_ptr| {
            let workers_idle =  node.workers_idle.load(Ordering::Relaxed);
            assert!(workers_idle > 0 && workers_idle < node.workers().len());
            let worker = unsafe { &*worker_ptr.as_ptr() };
            self.idle_workers = worker.next.get();
            node.workers_idle.store(workers_idle - 1, Ordering::Release);
            worker
        })
    }
}

struct Thread<P: Platform> {
    next: Cell<Option<NonNull<Self>>>,
}

#[derive(Default)]
pub struct Worker {
    next: Cell<Option<NonNull<Self>>>,
    node_ptr: usize,
    run_queue: LocalQueue,
}
