//! Numa-Aware, Multithreaded Scheduler based on
//! https://docs.google.com/document/u/0/d/1d3iI2QWURgDIsSR6G2275vMeQ_X7w-qxM2Vp7iGwwuM/pub.

use crate::runtime::{
    platform::Platform,
    task::{GlobalQueue, Kind, List, LocalQueue, Task},
    with_executor, with_executor_as, Executor,
};
use core::{
    any::Any,
    cell::Cell,
    num::NonZeroUsize,
    ptr::NonNull,
    slice::from_raw_parts,
    sync::atomic::{AtomicUsize, Ordering},
};
use yaar_lock::{
    sync::{CoreMutex, RawMutex},
    ThreadEvent,
};

pub enum RunError {
    /// No nodes were provided for the executor to run the future.
    EmptyNodes,
    /// A node with the slice index was found to not have any workers.
    NodeWithoutWorkers(usize),
    /// The starting node index was not in range for the provide nodes.
    InvalidStartNode,
}

pub struct NodeExecutor<P: Platform> {
    /// A reference to the platform instance
    platform: NonNull<P>,
    /// Pointer to the slice of nodes passed in from the run functions.
    nodes_ptr: NonNull<NonNull<Node<P>>>,
    /// Length of the slice of nodes passed in from the run functions.
    nodes_len: usize,
    /// The next node in which to schedule Kind::Parent tasks onto in the node
    /// slice.
    next_node: AtomicUsize,
    /// Synchronization event used to wait until all threads have exit.
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

    /// Run a task and any sub-tasks it recursively spawns using a slice of
    /// [`Node`]s for execution, scheduling the task onto the Node with the
    /// `start_node` index at the beginning and using an instance of the
    /// platform for actions which would normally interact with the
    /// operating system.
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
                    node.executor
                        .set(NonNull::new(executor as *const _ as *mut _));
                    if node.workers().len() == 0 {
                        return Err(RunError::NodeWithoutWorkers(index));
                    }

                    // initialize the workers
                    let mut worker_pool = node.worker_pool.lock();
                    for (index, worker) in node.workers().iter().enumerate() {
                        worker.node.set(NonNull::new(node as *const _ as *mut _));
                        worker_pool.put_worker(node, worker);
                        worker.id.set(index);
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
                self.platform()
                    .set_tls(thread_ptr.as_ref() as *const _ as usize);
                Some(&*thread_ptr.as_ptr())
            } else {
                NonNull::new(self.platform().get_tls() as *mut Thread<P>).map(|ptr| &*ptr.as_ptr())
            }
        }
    }

    /// Run the function with a reference to the current running
    /// [`NodeExecutor`] if any.
    fn with_current<T>(f: impl FnOnce(&Self) -> T) -> Option<T> {
        with_executor(|executor| (executor as *const Self).map(f)).flatten()
    }

    /// Run the function with a reference to the current running [`Thread`] if
    /// any.
    fn with_current_thread<T>(f: impl FnOnce(&Thread<P>) -> T) -> Option<T> {
        Self::with_current(|executor| executor.current_thread(None).map(f)).flatten()
    }

    /// Run the function with a reference to the current running [`Worker`] if
    /// any.
    fn with_current_worker<T>(f: impl FnOnce(&Worker<P>) -> T) -> Option<T> {
        Self::with_current_thread(|thread| thread.worker.get().map(|ptr| f(unsafe { &*ptr.as_ptr() })))
            .flatten()
    }

    /// Run the function with a reference to the [`Platform::ThreadLocalData`]
    /// of the current [`Node`] if the caller is currently running inside a
    /// [`NodeExecutor`].
    pub fn with_thread_local_data<T>(f: impl FnOnce(&P::ThreadLocalData) -> T) -> Option<T> {
        Self::with_current_thread(|thread| f(&thread.data))
    }

    /// Run the function with a reference to the [`Platform::WorkerLocalData`]
    /// of the current [`Node`] if the caller is currently running inside a
    /// [`NodeExecutor`].
    pub fn with_worker_local_data<T>(f: impl FnOnce(&P::WorkerLocalData) -> T) -> Option<T> {
        Self::with_current_worker(|worker| f(&worker.data))
    }

    /// Run the function with a reference to the [`Platform::NodeLocalData`] of
    /// the current [`Node`] if the caller is currently running inside a
    /// [`NodeExecutor`].
    pub fn with_node_local_data<T>(f: impl FnOnce(&P::NodeLocalData) -> T) -> Option<T> {
        Self::with_current_worker(|worker| worker.node().map(|node| f(node.data()))).flatten()
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
            }
            // Enqueue child tasks locally to the worker on this thread's node.
            Kind::Child => {
                Self::with_current_worker(|worker| {
                    let node = worker
                        .node()
                        .expect("Trying to schedule a task on a Worker not tied to a Node");
                    worker.run_queue.push(task, &node.run_queue);
                    node.spawn_worker();
                })
                .expect("Trying to schedule a task on a thread without a Worker");
            }
        }
    }
}

pub struct Node<P: Platform> {
    /// The ID of the node (e.g. NUMA Node Number).
    id: usize,
    /// Arbitrary data the platform can inject into nodes.
    data: P::NodeLocalData,
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
    /// A bitmask representing the platform's CPUs this node can bind threads
    /// to.
    cpu_affinity: Option<P::CpuAffinity>,
    /// A FIFO, linked list of tasks available to all workers/threads on this
    /// node.
    run_queue: GlobalQueue<CoreMutex<P::ThreadEvent>>,
}

impl<P: Platform> Node<P> {
    /// Get the node's id set on creation
    #[inline]
    pub fn id(&self) -> usize {
        self.id
    }

    /// Get a reference to the node's arbitrary data set on creation
    #[inline]
    pub fn data(&self) -> &P::NodeLocalData {
        &self.data
    }

    /// Get the array of Workers passed into the new function.
    fn workers(&self) -> &[Worker<P>] {
        unsafe { from_raw_parts(self.workers_ptr.as_ptr() as *const _, self.workers_len) }
    }

    /// Get the executor that this node is running on.
    fn executor(&self) -> &NodeExecutor<P> {
        unsafe { &*self.executor.get().unwrap().as_ptr() }
    }

    /// Try to spawn a new worker to handle system load of tasks.
    fn spawn_worker(&self) {
        // Only spawn a new worker if there arent any currently searching for work.
        // Failed to spawn a searching worker thread, reset the search count.
        if self
            .workers_searching
            .compare_and_swap(0, 1, Ordering::AcqRel)
            == 0
        {
            if self.worker_pool.lock().spawn_worker(self) {
                self.workers_searching.fetch_sub(1, Ordering::Release);
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
    fn put_worker(&mut self, node: &Node<P>, worker: &Worker<P>) {
        worker.next.set(self.idle_workers);
        self.idle_workers = NonNull::new(worker as *const _ as *mut _);
        let workers_idle = node.workers_idle.load(Ordering::Relaxed);
        node.workers_idle.store(workers_idle + 1, Ordering::Relaxed);
    }

    /// Try to get an idle worker from the idle worker cache.
    fn get_worker<'a>(&mut self, node: &Node<P>) -> Option<&'a Worker<P>> {
        self.idle_workers.map(|worker| {
            let worker = unsafe { &*worker.as_ptr() };
            self.idle_workers = worker.next.get();
            let workers_idle = node.workers_idle.load(Ordering::Relaxed);
            node.workers_idle.store(workers_idle - 1, Ordering::Relaxed);
            worker
        })
    }

    /// Mark a thread as idle by putting it in the idle thread cache.
    fn put_thread(&mut self, _node: &Node<P>, thread: &Thread<P>) {
        thread.next.set(self.idle_threads);
        self.idle_threads = NonNull::new(thread as *const _ as *mut _);
    }

    /// Spawn a new worker if possible on the provided node using a cached
    /// thread or creating a new thread.
    fn spawn_worker(&mut self, node: &Node<P>) -> bool {
        self.get_worker(node)
            .map(|worker| {
                // try and use a thread in the idle queue
                if let Some(thread) = self.idle_threads {
                    let thread = unsafe { &*thread.as_ptr() };
                    self.idle_threads = thread.next.get();
                    thread
                        .worker
                        .set(NonNull::new(worker as *const _ as *mut _));
                    thread.event.set();
                    return true;
                }

                // try to spawn a new thread
                if self.free_threads != 0 {
                    if node.executor().platform().spawn_thread(
                        node,
                        worker,
                        &node.cpu_affinity,
                        worker as *const _ as usize,
                        Thread::<P>::run,
                    ) {
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
    /// Thread acts as a Linked list
    next: Cell<Option<NonNull<Self>>>,
    /// Arbitrary data the platform can inject into threads.
    data: P::ThreadLocalData,
    /// The worker the thread is using to run tasks.
    /// If None, thread is either idle or should exit.
    worker: Cell<Option<NonNull<Worker<P>>>>,
    /// State which tracks if the worker/thread is searching for work from
    /// others.
    is_searching: Cell<bool>,
    /// Synchronization event to park the thread (for caching purposes &
    /// blocking in the future).
    event: P::ThreadEvent,
}

impl<P: Platform> Thread<P> {
    /// Entry point for running a worker on a [`Platform`] thread.
    pub extern "C" fn run(worker: usize) {
        // Get a reference to the node for this thread.
        let node = unsafe {
            &*(&*(worker as *mut Worker<P>))
                .node
                .get()
                .expect("Worker passed in without an associated Node")
                .as_ptr()
        };

        // Create this thread on the stack.
        let this = Self {
            next: Cell::new(None),
            data: P::ThreadLocalData::default(),
            worker: Cell::new(NonNull::new(worker as *mut Worker<P>)),
            is_searching: Cell::new(node.workers_searching.load(Ordering::Acquire) != 0),
            event: P::ThreadEvent::default(),
        };

        let mut tick = 0;
        while let Some(task) = this.poll(tick, node) {
            // When a worker finds a task and it was searching,
            // decrement the searching count and spawn a new worker
            // if it was the last to come out searching in order to
            // eventually maximize CPU utilization.
            if this.is_searching.get() {
                this.is_searching.set(false);
                if node.workers_searching.fetch_sub(1, Ordering::Release) == 1 {
                    node.spawn_worker();
                }
            }

            // execute the task & forward the tick.
            unsafe { task.resume() };
            tick = tick.wrapping_add(1);
        }

        // TODO: synchronize stop_event notification using
        // Node<P>.WorkerPool(inc(free_thread) == max_thread).
        // TODO: look into idling more threads / delayed thread stop.
    }

    fn poll<'a>(&self, tick: usize, node: &Node<P>) -> Option<&'a Task> {
        // TODO:
        //
        // if no worker: return None
        //
        // poll timers (mutex locked)
        //   - if empty, return time until one expires or null (wait_time)
        // poll local queue:
        //   - if tick % 64: poll 1 from global
        //   - if tick % 32: poll_front from local queue
        //   - poll (back) from local queue
        // poll many from global queue
        // poll reactor (non blocking)
        // poll others:
        //   - inc searching if searching < (num_workers - workers_idle)
        //   - set thread.is_searching
        //   - fn steal_from(node):
        //     - iterate workers in random order
        //     - try_lock steal a timer from worker if expired.
        //     - try steal from worker's local queue
        //   - steal_from(node.workers)
        //   - steal_from(for n in nodes if n != node)
        // poll many from global queue
        //
        // give up worker, stop is_search (dec searching - AcqRel)
        // (for below) if was thread.is_searching, restore it (inc searching - Acq) if
        // found task for w in node.workers: if timers(w) or localq(w):
        //   - grab idle worker, if none, break loop
        //
        // try_lock poll reactor (blocking for wait_time)
        //   (reactor tracks waiting, if no waiting, poll = none found)
        //   - if none found & wait_time, acquire worker & re-loop
        //   - if none found, return None
        //   - if 1 found, acquire worker, return 1
        //   - if many found, pop 1, acquire worker, submit reset, return 1
        //
        // give up thread, wait for event, re-loop
        None
    }
}

pub struct Worker<P: Platform> {
    /// An identifier which represents the worker offset in a [`Node`]
    id: Cell<usize>,
    /// Arbitrary data the platform can inject into workers.
    data: P::WorkerLocalData,
    /// Worker acts as a Linked list
    next: Cell<Option<NonNull<Self>>>,
    /// Pointer reference to the node the worker belongs to.
    node: Cell<Option<NonNull<Node<P>>>>,
    /// Local list of runnable tasks that can be stolen from.
    run_queue: LocalQueue,
}

impl<P: Platform> Worker<P> {
    /// Get a reference to the workers [`Node`] if any.
    fn node<'a>(&self) -> Option<&'a Node<P>> {
        self.node.get().map(|ptr| unsafe { &*ptr.as_ptr() })
    }
}
