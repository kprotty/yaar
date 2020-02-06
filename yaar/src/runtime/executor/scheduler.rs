//! Numa-Aware, Multithreaded Scheduler based on
//! https://docs.google.com/document/u/0/d/1d3iI2QWURgDIsSR6G2275vMeQ_X7w-qxM2Vp7iGwwuM/pub.

use crate::{
    runtime::{
        platform::Platform,
        task::{GlobalQueue, LocalQueue, Task, Kind, List},
        Executor, with_executor_as,
    },
};
use core::{
    num::NonZeroUsize,
    ptr::NonNull,
    cell::Cell,
    slice::from_raw_parts,
    sync::atomic::{Ordering, AtomicUsize},
};
use yaar_lock::{ThreadEvent, sync::{RawMutex, CoreMutex}};

pub enum RunError {
    /// No nodes were provided for the executor to run the future.
    EmptyNodes,
    /// A node with the slice index was found to not have any workers.
    NodeWithoutWorkers(usize),
    /// The starting node index was not in range for the provide nodes.
    InvalidStartNode,
}

pub struct NodeExecutor<P: Platform> {
    platform: NonNull<P>,
    nodes_ptr: NonNull<NonNull<Node<P>>>,
    nodes_len: usize,
    next_node: AtomicUsize,
    stop_event: P::ThreadEvent,
}

impl<P: Platform> NodeExecutor<P> {
    /// TODO: Run the task with an executor optimized for single threaded
    /// access.
    pub fn run_serial(_platform: &P, _task: &Task) {
        unimplemented!();
    }

    /// TODO: Run the task with a scheme akin to well-known executors such as
    /// tokio and async-std with a unified SMP thread-pool over multiple Nodes.
    pub fn run_smp(
        _platform: &P,
        _workers: &[Worker<P>],
        _max_threads: NonZeroUsize,
        _task: &Task,
    ) {
        unimplemented!();
    }

    pub fn run_using(
        platform: &P,
        start_node: usize,
        nodes: &[NonNull<Node<P>>],
        task: &Task,
    ) -> Result<(), RunError> {
        if nodes.len() == 0 {
            return Err(RunError::EmptyNodes);
        } else if start_node >= nodes.len() {
            return Err(RunError::InvalidStartNode);
        }

        with_executor_as(
            &Self {
                platform: NonNull::new(platform as *const _ as *mut _).unwrap(),
                nodes_ptr: NonNull::new(nodes.as_ptr() as *mut _).unwrap(),
                nodes_len: nodes.len(),
                next_node: AtomicUsize::new(0),
                stop_event: P::ThreadEvent::default(),
            },
            |executor| {
                // initialize the nodes
                for (index, node) in executor.nodes().iter().enumerate() {
                    node.executor.set(NonNull::new(executor as *const _ as *mut _));
                    if node.workers().len() == 0 {
                        return Err(RunError::NodeWithoutWorkers(index));
                    }

                    // initialize the workers
                    let mut worker_pool = node.worker_pool.lock();
                    for worker in node.workers() {
                        worker.node.set(NonNull::new(node as *const _ as *mut _));
                        worker_pool.put_worker(node, worker);
                    }
                }
                
                // TODO
                let main_node = executor.nodes()[start_node];
                let worker_pool = main_node.worker_pool.lock();
                Ok(())
            },
        )
    }

    /// Get the array of Nodes passed into the run function.
    fn nodes(&self) -> &[&Node<P>] {
        unsafe { from_raw_parts(self.nodes_ptr.as_ptr() as *const _, self.nodes_len) }
    }

    /// Get the reference to the platform passed into the run function.
    fn platform(&self) -> &P {
        unsafe { &*self.platform.as_ptr() }
    }

    /// Get or set the current [`Thread`] based on thread local storage.
    fn current_thread<'a>(&self, set: Option<NonNull<Thread<P>>>) -> Option<&'a Thread<P>> {
        unsafe {
            if let Some(thread_ptr) = set {
                self.platform().set_tls(thread_ptr.as_ref() as *const _ as usize);
                Some(&*thread_ptr.as_ptr())
            } else {
                NonNull::new(self.platform().get_tls() as *mut Thread<P>)
                    .map(|ptr| &*ptr.as_ptr())
            }
        }
    }
}

unsafe impl<P: Platform> Sync for NodeExecutor<P> {}

impl<P: Platform> Executor for NodeExecutor<P> {
    fn schedule(&self, task: &Task) {
        match task.kind() {
            // Distribute root/parent tasks round-robin across nodes.
            // The task then becomes a child to that node so its not re-distributed.
            Kind::Parent => {
                let next_node = self.next_node.fetch_add(1, Ordering::Relaxed);
                let node = self.nodes()[next_node % self.nodes().len()];
                node.run_queue.push({
                    let mut list = List::default();
                    unsafe { task.set_kind(Kind::Child) };
                    list.push(task);
                    list
                });
                node.spawn_worker();
            },
            // Enqueue child tasks locally to the worker on this thread's node.
            Kind::Child => unsafe {
                let thread = self
                    .current_thread(None)
                    .expect("Trying to schedule task on a thread not owned by NodeExecutor.");
                let worker = thread.worker
                    .get()
                    .map(|worker_ptr| &*worker_ptr.as_ptr())
                    .expect("Trying to schedule a task on a thread with no Worker.");
                let node = worker.node
                    .get()
                    .map(|node_ptr| &*node_ptr.as_ptr())
                    .expect("Trying to schedule a task on a Worker not tied to a Node.");
                worker.run_queue.push(task, &node.run_queue);
                node.spawn_worker();
            }
        }
    }
}

pub struct Node<P: Platform> {
    /// The ID of the node (e.g. NUMA Node Number).
    id: usize,
    /// A reference to the executor which manages all nodes.
    executor: Cell<Option<NonNull<NodeExecutor<P>>>>,
    /// Pointer to the slice of workers this node owns.
    workers_ptr: NonNull<Worker<P>>,
    /// The length for the slice of workers this node owns.
    workers_len: usize,
    /// Loose tracking for the number of workers not running on threads.
    workers_idle: AtomicUsize,
    /// Counter which tracks the amount of workers trying to steal from others.
    workers_searching: AtomicUsize,
    /// A pool of Workers & Threads available to the Node to executue tasks.
    worker_pool: RawMutex<WorkerPool<P>, P::ThreadEvent>,
    /// A bitmask representing the platform CPUs this node should bind threads to.
    cpu_affinity: Option<P::CpuAffinity>,
    /// A FIFO, linked list of tasks available to all workers/threads on this node.
    run_queue: GlobalQueue<CoreMutex<P::ThreadEvent>>,
}

impl<P: Platform> Node<P> {
    /// Get the array of Workers passed into the new function.
    pub(super) fn workers(&self) -> &[Worker<P>] {
        unsafe { from_raw_parts(self.workers_ptr.as_ptr() as *const _, self.workers_len) }
    }

    /// Get the executor that this node is running on.
    pub(super) fn executor(&self) -> &NodeExecutor<P> {
        unsafe { &*self.executor.get().unwrap().as_ptr() }
    }

    /// Try to spawn a new worker to handle system load of tasks.
    pub(super) fn spawn_worker(&self) {
        // Only spawn a new worker if there arent any currently searching for work.
        // Failed to spawn a searching worker thread, reset the search count.
        if self.workers_searching.compare_and_swap(0, 1, Ordering::AcqRel) == 0 {
            if self.worker_pool.lock().spawn_worker(self) {
                self.workers_searching.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
}

struct WorkerPool<P: Platform> {
    /// The number of threads available to be spawned.
    free_threads: usize,
    /// The maximum amount of spawnable platform threads.
    /// `max_threads` - `free_threads` = currently active threads.
    max_threads: NonZeroUsize,
    /// Linked list stack of threads which are waiting for a new worker.
    idle_threads: Option<NonNull<Thread<P>>>,
    /// Linked list stack of workers which don't have any tasks to run.
    idle_workers: Option<NonNull<Worker<P>>>,
}

impl<P: Platform> WorkerPool<P> {
    /// Mark a worker as idle by putting it in the idle worker cache.
    pub fn put_worker(&mut self, node: &Node<P>, worker: &Worker<P>) {
        worker.next.set(self.idle_workers);
        self.idle_workers = NonNull::new(worker as *const _ as *mut _);
        let workers_idle = node.workers_idle.load(Ordering::Relaxed);
        node.workers_idle.store(workers_idle + 1, Ordering::Relaxed);
    }

    /// Try to get an idle worker from the idle worker cache.
    pub fn get_worker<'a>(&mut self, node: &Node<P>) -> Option<&'a Worker<P>> {
        self.idle_workers.map(|worker| {
            let worker = unsafe { &*worker.as_ptr() };
            self.idle_workers = worker.next.get();
            let workers_idle = node.workers_idle.load(Ordering::Relaxed);
            node.workers_idle.store(workers_idle - 1, Ordering::Relaxed);
            worker
        })
    }

    /// Mark a thread as idle by putting it in the idle thread cache.
    pub fn put_thread(&mut self, _node: &Node<P>, thread: &Thread<P>) {
        thread.next.set(self.idle_threads);
        self.idle_threads = NonNull::new(thread as *const _ as *mut _);
    }

    /// Spawn a new worker if possible on the provided node using a cached thread or creating a new thread.
    pub fn spawn_worker(&mut self, node: &Node<P>) -> bool {
        self.get_worker(node)
            .map(|worker| {
                // try and use a thread in the idle queue
                if let Some(thread) = self.idle_threads {
                    let thread = unsafe { &*thread.as_ptr() };
                    self.idle_threads = thread.next.get();
                    thread.worker.set(NonNull::new(worker as *const _ as *mut _));
                    thread.event.set();
                    return true;
                }

                // try to spawn a new thread
                if self.free_threads != 0 {
                    if node.executor()
                        .platform()
                        .spawn_thread(
                            node.id,
                            &node.cpu_affinity,
                            worker as *const _ as usize,
                            Thread::<P>::run,
                        )
                    {
                        self.free_threads -= 1;
                        return true;
                    }
                }

                // No idle threads and unable to spawn a new one
                false
            })
            .unwrap_or(false)
    }
}

struct Thread<P: Platform> {
    next: Cell<Option<NonNull<Self>>>,
    worker: Cell<Option<NonNull<Worker<P>>>>,
    event: P::ThreadEvent,
}

impl<P: Platform> Thread<P> {
    /// Entry point for running a worker on a [`Platform`] thread.
    pub extern "C" fn run(worker: usize) {
        // Get a reference to the node for this thread.
        let _node = unsafe {
            let worker = &*(worker as *mut Worker<P>);
            &*worker.node.get().unwrap().as_ptr()
        };

        // Create this thread on the stack.
        let _this = Self {
            next: Cell::new(None),
            worker: Cell::new(NonNull::new(worker as *mut Worker<P>)),
            event: P::ThreadEvent::default(),
        };


    }
}

pub struct Worker<P: Platform> {
    next: Cell<Option<NonNull<Self>>>,
    node: Cell<Option<NonNull<Node<P>>>>,
    run_queue: LocalQueue,
}
