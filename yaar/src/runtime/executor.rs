use super::{CachedFutureTask, Platform, Task, TaskPriority};
use core::{
    cell::{Cell, RefCell},
    convert::TryInto,
    future::Future,
    mem::{transmute, MaybeUninit},
    num::{NonZeroU64, NonZeroUsize},
    ptr::NonNull,
    sync::atomic::{spin_loop_hint, AtomicUsize, Ordering},
};
use yaar_lock::sync::{RawMutex, ThreadParker};

/// Run a future along with any other [`Task`]s it recursively spawns
/// until completion. This is similar to `block_on` found in other executors.
///
/// The runtime does not assume much about the system and uses the caller
/// provided options for configuration in order to run the future and its tasks:
///
/// * `platform`: A reference to an interface for platform-depedent procedures
///    such as spawning new threads or interacting with thread-local storage.
///
/// * `workers`: Array of workers, possibly uninitialized, which are used to
///    execute runtime [`Task`] concurrently or in parallel. The runtime will
///
/// * `max_threads`: The maximum amount of threads the runtime is allowed to
///    spawn using the platform, excluding 1 for the main thread. The runtime
///    will spawn a new thread per worker but may spawn more for handling
///    tasks which block too long (see: [`run_blocking`]).
///
/// [`run_blocking`]: struct.Task.html
pub fn run<P, T>(
    platform: &P,
    workers: &[Worker],
    max_threads: NonZeroUsize,
    future: impl Future<Output = T>,
) -> T
where
    P: Platform,
    P::Parker: ThreadParker,
{
    // If no workers provided, allocate one on the stack
    if workers.len() == 0 {
        let worker = Worker::default();
        let workers = core::slice::from_ref(&worker);
        Executor::run(platform, workers, max_threads, future)
    } else {
        Executor::run(platform, workers, max_threads, future)
    }
}

// A global reference to the current ExecutorRef
static EXECUTOR_CELL: ExecutorCell = ExecutorCell(Cell::new(None));
struct ExecutorCell(Cell<Option<NonNull<ExecutorRef>>>);
unsafe impl Sync for ExecutorCell {}

/// Virtual reference to an `Executor`
pub(super) struct ExecutorRef {
    pub ptr: *const (),
    pub schedule: fn(*const (), *mut Task),
}

/// Get a reference to the current running executor
pub(super) fn get_executor_ref<'a>() -> Option<&'a ExecutorRef> {
    EXECUTOR_CELL.0.get().map(|ptr| unsafe { &*ptr.as_ptr() })
}

struct Executor<'a, P>
where
    P: Platform,
    P::Parker: ThreadParker,
{
    platform: &'a P,
    workers: &'a [Worker],
    stop_parker: P::Parker,
    workers_idle: AtomicUsize,
    workers_stealing: AtomicUsize,
    injector: RawMutex<TaskInjector, P::Parker>,
    worker_pool: RawMutex<WorkerPool<'a, P>, P::Parker>,
}

impl<'a, P> Executor<'a, P>
where
    P: Platform,
    P::Parker: ThreadParker,
{
    pub fn run<T>(
        platform: &'a P,
        workers: &'a [Worker],
        max_threads: NonZeroUsize,
        future: impl Future<Output = T>,
    ) -> T {
        let executor = Self {
            platform,
            workers,
            stop_parker: P::Parker::default(),
            workers_idle: AtomicUsize::new(0),
            workers_stealing: AtomicUsize::new(0),
            injector: RawMutex::new(TaskInjector::default()),
            worker_pool: RawMutex::new(WorkerPool {
                max_threads: max_threads.get(),
                free_threads: max_threads.get(),
                idle_threads: None,
                idle_workers: None,
            }),
        };

        // virtual reference to the current executor
        let executor_ref = ExecutorRef {
            ptr: &executor as *const Self as *const (),
            schedule: |ptr, task| {
                unsafe { (&*(ptr as *const Self)).schedule(&mut *task) };
            },
        };

        // push our executor_ref pointer to the executor cell stack & call the function
        let ref_ptr = NonNull::new(&executor_ref as *const _ as *mut _);
        let old_ref = EXECUTOR_CELL.0.replace(ref_ptr);

        // capture the result of the given future
        let result = {
            let mut future = CachedFutureTask::new(TaskPriority::Normal, future);
            executor.schedule(future.as_mut());

            // prepare the workers & use the main thread to run one.
            Thread::<'a, P>::run({
                let mut pool = executor.worker_pool.lock();
                for worker in executor.workers.iter() {
                    worker.executor.set(&executor as *const _ as usize);
                    pool.put_worker(&executor, worker);
                }
                pool.free_threads -= 1;
                pool.find_worker(&executor).unwrap() as *const _ as usize
            });

            // wait for all threads to stop and return the future result
            executor.stop_parker.park();
            future
                .into_inner()
                .expect("The provided future hit a deadlock")
        };

        // store what was originally in the executor cell stack & ensure our context was popped.
        let our_ref = EXECUTOR_CELL.0.replace(old_ref);
        debug_assert_eq!(our_ref, ref_ptr);
        result
    }

    fn schedule(&self, task: &mut Task) {
        // Try to push to the current worker from thread-local storage
        NonNull::new(self.platform.get_tls() as *mut Thread<'a, P>)
            .and_then(|ptr| unsafe { (&*ptr.as_ptr()).worker.get() })
            .map(|worker| worker.run_queue.borrow_mut().push(task, &self.injector))
            // No worker available, just push to global queue / injector
            .unwrap_or_else(|| {
                let mut list = TaskList::default();
                list.push(task);
                self.injector.lock().push(list);
            });

        // spawn a new worker to potentially handle this new task
        if self.workers_stealing.load(Ordering::Acquire) == 0 {
            let _ = self.try_spawn_worker();
        }
    }

    fn try_spawn_worker(&self) -> bool {
        // only spawn a worker if there aren't any stealing workers
        // already as they would steal the tasks meant for this one
        if self
            .workers_stealing
            .compare_and_swap(0, 1, Ordering::Acquire)
            != 0
        {
            return false;
        }

        // try and spawn a new thread for an idle worker
        let mut pool = self.worker_pool.lock();
        if let Some(worker) = pool.find_worker(self) {
            if pool.find_thread_for(self, worker) {
                return true;
            } else {
                pool.put_worker(self, worker);
            }
        }

        // failed to find a thread for the worker, fix the stealing count.
        self.workers_stealing.fetch_sub(1, Ordering::Release);
        false
    }
}

struct WorkerPool<'a, P>
where
    P: Platform,
    P::Parker: ThreadParker,
{
    max_threads: usize,
    free_threads: usize,
    idle_threads: Option<&'a Thread<'a, P>>,
    idle_workers: Option<&'a Worker>,
}

impl<'a, P> WorkerPool<'a, P>
where
    P: Platform,
    P::Parker: ThreadParker,
{
    /// Mark a worker as idle as it has no more work to run.
    pub fn put_worker(&mut self, executor: &Executor<'a, P>, worker: &'a Worker) {
        debug_assert!(!worker.has_pending_tasks());
        let next = self
            .idle_workers
            .and_then(|w| NonNull::new(w as *const _ as *mut _));
        worker.next.set(next);
        self.idle_workers = Some(worker);
        executor.workers_idle.fetch_add(1, Ordering::Relaxed);
    }

    /// Find a free idle worker to use.
    pub fn find_worker(&mut self, executor: &Executor<'a, P>) -> Option<&'a Worker> {
        self.idle_workers.map(|worker| {
            self.idle_workers = worker.next.get().map(|ptr| unsafe { &*ptr.as_ptr() });
            executor.workers_idle.fetch_sub(1, Ordering::Relaxed);
            worker
        })
    }

    /// Mark a thread as idle as it lost it's worker and is waiting for work.
    pub fn put_thread(&mut self, _executor: &Executor<'a, P>, thread: &'a Thread<'a, P>) {
        thread.next.set(self.idle_threads);
        self.idle_threads = Some(thread);
    }

    /// Find an idle thread to use to process work from a given worker.
    pub fn find_thread_for(&mut self, executor: &Executor<'a, P>, worker: &'a Worker) -> bool {
        // first, check the idle thread free list
        if let Some(thread) = self.idle_threads {
            self.idle_threads = thread.next.get();
            thread.worker.set(Some(worker));
            thread.parker.unpark();
            return true;
        }

        // then, try to create a new thread using the executor underlying platform
        if self.free_threads != 0 {
            let worker = worker as *const Worker as usize;
            if executor.platform.spawn_thread(worker, Thread::<'a, P>::run) {
                self.free_threads -= 1;
                return true;
            }
        }

        // failed to spawn a new thread for the worker
        false
    }
}

struct Thread<'a, P>
where
    P: Platform,
    P::Parker: ThreadParker,
{
    next: Cell<Option<&'a Self>>,
    worker: Cell<Option<&'a Worker>>,
    is_stealing: Cell<bool>,
    parker: P::Parker,
}

impl<'a, P: 'a> Thread<'a, P>
where
    P: Platform,
    P::Parker: ThreadParker,
{
    /// Run a task scheduling [`Worker`] from a usize pointer on our thread.
    pub extern "C" fn run(worker: usize) {
        let worker = unsafe { &*(worker as *const Worker) };
        let executor = unsafe { &*(worker.executor.get() as *const Executor<'a, P>) };

        // allocate our thread structure on our stack
        let this = Self {
            next: Cell::new(None),
            worker: Cell::new(Some(worker)),
            is_stealing: Cell::new(executor.workers_stealing.load(Ordering::Relaxed) != 0),
            parker: P::Parker::default(),
        };

        let mut step = 0;
        executor.platform.set_tls(&this as *const Self as usize);
        while let Some(task) = this.poll(executor, step) {
            // if we were the last worker thread to come out of spinning,
            // start up a new spinning worker to eventually maximize distribution.
            if this.is_stealing.replace(false) {
                if executor.workers_stealing.fetch_sub(1, Ordering::Release) == 1 {
                    let _ = executor.try_spawn_worker();
                }
            }

            // execute the runnable task
            step = step.wrapping_add(1);
            unsafe { task.resume() };
        }

        // notify the executor that our thread is stopping.
        let mut pool = executor.worker_pool.lock();
        pool.free_threads += 1;

        // if we're the last thread, wake up any idle threads to
        // stop them as well then set the stop signal to end the executor.
        if pool.free_threads == pool.max_threads {
            while let Some(thread) = pool.idle_threads {
                pool.idle_threads = thread.next.get();
                thread.worker.set(None);
                thread.parker.unpark();
            }
            executor.stop_parker.unpark();
        }
    }

    /// Poll the entire executor for a new runnable task, returns None if there arent any.
    fn poll<'t>(&self, executor: &'a Executor<'a, P>, step: usize) -> Option<&'t mut Task> {
        'poll: loop {
            // can only poll for work if we have a worker
            let our_worker = match self.worker.get() {
                Some(worker) => worker,
                None => return None,
            };

            // Try to find a runnable task throughout the system
            let mut wait_time = None;
            if let Some(task) = Self::poll_timers(our_worker, &mut wait_time)
                .or_else(|| Self::poll_local(executor, our_worker, step))
                .or_else(|| Self::poll_global(executor, our_worker, 1))
                .or_else(|| Self::poll_io(executor, Some(0), self))
                .or_else(|| Self::poll_workers(executor, our_worker, self))
                .or_else(|| Self::poll_global(executor, our_worker, !0))
            {
                return Some(task);
            }

            // Give up our worker since we couldn't find any work in the system
            self.worker.set(None);
            executor.worker_pool.lock().put_worker(executor, our_worker);

            // Drop the stealing count with full barrier, then check all run queues again.
            // Doing it the other way around means a thread could schedule a task after we
            // checked all run queues but before we stopped stealing; possibly resulting
            // in no thread being woken up to handle that new task.
            //
            // After this point, if a new task is discovered below, we need to restore
            // our spinning state in order to wake up another worker thread after our
            // poll() inside Self::run().
            let was_stealing = self.is_stealing.get();
            if was_stealing {
                self.is_stealing.set(false);
                executor.workers_stealing.fetch_sub(1, Ordering::AcqRel);
            }

            // Check all run queues again
            for worker in executor.workers.iter() {
                if worker.has_pending_tasks() {
                    if let Some(worker) = executor.worker_pool.lock().find_worker(executor) {
                        self.worker.set(Some(worker));
                        if was_stealing {
                            self.is_stealing.set(true);
                            executor.workers_stealing.fetch_add(1, Ordering::Release);
                        }
                        continue 'poll;
                    } else {
                        break;
                    }
                }
            }

            // try to poll for io by blocking (acquires a worker if without one)
            if let Some(task) = Self::poll_io(executor, wait_time.map(|t| t.get()), self) {
                if was_stealing {
                    self.is_stealing.set(true);
                    executor.workers_stealing.fetch_add(1, Ordering::Release);
                }
                return Some(task);
            }

            // if blocking for poll didnt find any tasks, but theres a wait_time
            // then a timer may have expired. Grab a worker to try and see.
            if wait_time.is_some() {
                if let Some(worker) = executor.worker_pool.lock().find_worker(executor) {
                    self.worker.set(Some(worker));
                    if was_stealing {
                        self.is_stealing.set(true);
                        executor.workers_stealing.fetch_add(1, Ordering::Release);
                    }
                    continue 'poll;
                }
            }

            // There is no work in the system and another thread is polling for io.
            // We lost our worker, so move to the idle queue and wait to be woken up
            // with a possibly new worker to process tasks again.
            //
            // Need to extend our thread's lifetime to that of the executor since
            // our thread is allocated on our stack and doesnt form a regional heirarchy.
            let self_ref = unsafe { &*(self as *const Self) };
            executor.worker_pool.lock().put_thread(executor, self_ref);
            self.parker.park();
            self.parker.reset();
            continue 'poll;
        }
    }

    /// Check timers for runnable tasks.
    /// Updates the wait_time to the amount of ticks until
    /// a timer expires if any or leaves it as `None`.
    fn poll_timers<'t>(
        worker: &'a Worker,
        wait_time: &mut Option<NonZeroU64>,
    ) -> Option<&'t mut Task> {
        None // TODO
    }

    /// Check the worker's local queue for tasks,
    /// polling also the global injector for eventual faireness.
    fn poll_local<'t>(
        executor: &'a Executor<'a, P>,
        worker: &'a Worker,
        step: usize,
    ) -> Option<&'t mut Task> {
        None // TODO
    }

    /// Check the global injector queue for tasks,
    /// grabbing a batch into the worker's local queue when possible.
    fn poll_global<'t>(
        executor: &'a Executor<'a, P>,
        worker: &'a Worker,
        max_batch: usize,
    ) -> Option<&'t mut Task> {
        None // TODO
    }

    /// Poll for IO, blocking for `timoeut` ticks until a task is ready'd.
    /// Passing `None` for timeout blocks indefinitely for a task.
    /// If a thread has lost its worker, this function attempts to find one for it.
    fn poll_io<'t>(
        executor: &'a Executor<'a, P>,
        timeout: Option<u64>,
        thread: &Self,
    ) -> Option<&'t mut Task> {
        None // TODO
    }

    /// Check the run queues from other workers and try to steal from them (Work Stealing).
    fn poll_workers<'t>(
        executor: &'a Executor<'a, P>,
        worker: &'a Worker,
        thread: &Self,
    ) -> Option<&'t mut Task> {
        None // TODO
    }
}

#[derive(Default)]
pub struct Worker {
    next: Cell<Option<NonNull<Self>>>,
    executor: Cell<usize>,
    run_queue: RefCell<TaskQueue>,
}

impl Worker {
    pub(super) fn has_pending_tasks(&self) -> bool {
        // TODO: check timers
        self.run_queue.borrow().len() != 0
    }
}

/// An intrusive linked list of [`Task`]s
#[derive(Default)]
struct LinkedList {
    head: Option<NonNull<Task>>,
    tail: Option<NonNull<Task>>,
}

impl LinkedList {
    /// Pop one task from the front of the list.
    pub fn pop<'a>(&mut self) -> Option<&'a mut Task> {
        self.head.map(|task| {
            let task = unsafe { &mut *task.as_ptr() };
            self.head = task.next();
            if self.head.is_none() {
                self.tail = None;
            }
            task
        })
    }

    /// Push an entire list at once to the back of this list
    pub fn push(&mut self, list: Self) {
        if let Some(mut tail) = self.tail {
            unsafe { tail.as_mut().set_next(list.head) }
        }
        if self.head.is_none() {
            self.head = list.head;
        }
        self.tail = list.tail;
    }

    /// Push an entire list at once to the front of this list
    pub fn push_front(&mut self, list: Self) {
        if let Some(mut tail) = list.tail {
            unsafe { tail.as_mut().set_next(self.head) }
        }
        if self.tail.is_none() {
            self.tail = list.tail;
        }
        self.head = list.head;
    }
}

// A prioritized, intrusive linked list of [`Task`]s.
#[derive(Default)]
pub(super) struct TaskList {
    front: LinkedList,
    back: LinkedList,
    size: usize,
}

impl TaskList {
    /// Get the size of the task list
    #[inline]
    pub fn len(&self) -> usize {
        self.size
    }

    /// Push the task to the internal list, ordering it based on it's priority
    pub fn push(&mut self, task: &mut Task) {
        task.set_next(None);
        let list = LinkedList {
            head: NonNull::new(task),
            tail: NonNull::new(task),
        };

        self.size += 1;
        match task.priority() {
            TaskPriority::Low | TaskPriority::Normal => self.back.push(list),
            TaskPriority::High => self.front.push(list),
        }
    }
}

#[derive(Default)]
struct TaskInjector {
    list: LinkedList,
    size: usize,
}

impl TaskInjector {
    pub fn push(&mut self, list: TaskList) {
        let TaskList { front, back, size } = list;
        if size != 0 {
            self.list.push_front(front);
            self.list.push(back);
            self.size += size;
        }
    }

    pub fn pop<'a>(
        &mut self,
        queue: &mut TaskQueue,
        partitions: NonZeroUsize,
        max: usize,
    ) -> Option<&'a mut Task> {
        if self.size == 0 {
            return None;
        }

        // Safe to "unwrap()" since size should be guaranteed by push() above
        let mut size = ((self.size / partitions.get()) + 1)
            .max(max)
            .max(TaskQueue::SIZE / 2);
        debug_assert!(size != 0);
        self.size -= size;
        let task = self.list.pop();

        // push all the other tasks without synchronization.
        size -= 1;
        if size != 0 {
            let mut tail = queue.unsync_load_tail();
            let mut head = queue.head().load(Ordering::Relaxed);
            debug_assert_eq!(
                tail.wrapping_sub(head),
                0,
                "Attempting to pop many from injector with a non-empty queue"
            );

            // move the batch of tasks into the provided queue
            for _ in 0..size {
                let task = match self.list.pop() {
                    Some(task) => task,
                    None => break,
                };
                match task.priority() {
                    TaskPriority::Low | TaskPriority::Normal => {
                        queue.tasks[(tail as usize) % TaskQueue::SIZE] = MaybeUninit::new(task);
                        tail = tail.wrapping_add(1);
                    }
                    TaskPriority::High => {
                        head = head.wrapping_sub(1);
                        queue.tasks[(head as usize) % TaskQueue::SIZE] = MaybeUninit::new(task);
                    }
                }
            }

            // Make the new tasks in the queue available to itself & consumers
            queue
                .pos
                .store(TaskQueue::to_pos(head, tail), Ordering::Release);
        }

        task
    }
}

/// A Bounded Multi-Consumer Single-Producer Queue
/// of tasks which supports batched stealing.
struct TaskQueue {
    pos: AtomicUsize,
    tasks: [MaybeUninit<*mut Task>; Self::SIZE],
}

impl Default for TaskQueue {
    fn default() -> Self {
        Self {
            pos: AtomicUsize::new(0),
            tasks: [MaybeUninit::uninit(); Self::SIZE],
        }
    }
}

#[cfg(target_pointer_width = "64")]
type TaskQueueIndex = u32;
#[cfg(target_pointer_width = "64")]
type TaskQueueAtomicIndex = core::sync::atomic::AtomicU32;

#[cfg(target_pointer_width = "32")]
type TaskQueueIndex = u16;
#[cfg(target_pointer_width = "32")]
type TaskQueueAtomicIndex = core::sync::atomic::AtomicU16;

impl TaskQueue {
    /// The size of the TaskQueue ring buffer
    pub const SIZE: usize = 256;

    /// Get a reference to the atomic head index of the TaskQueue ring buffer
    #[inline(always)]
    pub fn head(&self) -> &TaskQueueAtomicIndex {
        &self.indices()[0]
    }

    /// Get a reference to the atomic tail index of the TaskQueue ring buffer
    #[inline(always)]
    pub fn tail(&self) -> &TaskQueueAtomicIndex {
        &self.indices()[1]
    }

    /// Split the atomic pos field into two for head & tail.
    /// Safe to do since `pos` acts as a C union.
    #[inline(always)]
    fn indices(&self) -> &[TaskQueueAtomicIndex; 2] {
        unsafe { &*(&self.pos as *const _ as *const _) }
    }

    /// Convert a head index and a tail index into a single value for `pos`.
    pub fn to_pos(head: TaskQueueIndex, tail: TaskQueueIndex) -> usize {
        unsafe { transmute([head, tail]) }
    }

    /// Load the tail index without any synchronization.
    /// Safe to do so since it requires ownership and other threads cannot modify the tail.
    pub fn unsync_load_tail(&mut self) -> TaskQueueIndex {
        let tail_ref = self.tail() as *const _ as *mut TaskQueueAtomicIndex;
        unsafe { *(&mut *tail_ref).get_mut() }
    }

    /// Returns the number of tasks in the queue
    pub fn len(&self) -> usize {
        let tail = self.tail().load(Ordering::Acquire);
        let head = self.head().load(Ordering::Acquire);
        tail.wrapping_sub(head) as usize
    }

    /// Enqueue a task to the front or back of the ring buffer based on priority.
    /// Overflows half of the queue into injector when full.
    pub fn push<P: ThreadParker>(&mut self, task: &mut Task, injector: &RawMutex<TaskInjector, P>) {
        match task.priority() {
            TaskPriority::Low | TaskPriority::Normal => self.push_back(task, injector),
            TaskPriority::High => self.push_front(task, injector),
        }
    }

    /// Enqueue a task to the back of the ring buffer task queue
    fn push_back<P: ThreadParker>(
        &mut self,
        task: &mut Task,
        injector: &RawMutex<TaskInjector, P>,
    ) {
        loop {
            // unsychronized loads for both since:
            // - the tail is only modified by the queue owner
            // - needs no write visibility (Acquire) to self.tasks
            let tail = self.unsync_load_tail();
            let head = self.head().load(Ordering::Relaxed);

            // queue is not full, use Release to make self.tasks write available to stealers.
            if (tail.wrapping_sub(head) as usize) < Self::SIZE {
                self.tasks[(tail as usize) % Self::SIZE] = MaybeUninit::new(task);
                self.tail().store(tail.wrapping_add(1), Ordering::Release);
                return;
            }

            // queue is full, try to overflow into injector
            if self.push_overflow(task, head, injector) {
                return;
            } else {
                spin_loop_hint();
            }
        }
    }

    /// Enqueue a task to the front of the ring buffer task queue
    fn push_front<P: ThreadParker>(
        &mut self,
        task: &mut Task,
        injector: &RawMutex<TaskInjector, P>,
    ) {
        loop {
            // unsynchronized load on the tail since only we can modify it,
            // Relaxed load on the tail since not reading any self.tasks updates from other threads.
            let tail = self.unsync_load_tail();
            let head = self.head().load(Ordering::Relaxed);

            // queue is not full, use Release to make self.tasks write available to stealers.
            if (tail.wrapping_sub(head) as usize) < Self::SIZE {
                let new_head = head.wrapping_sub(1);
                self.tasks[(new_head as usize) % Self::SIZE] = MaybeUninit::new(task);
                match self.head().compare_exchange_weak(
                    head,
                    new_head,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(_) => {
                        spin_loop_hint();
                        continue;
                    }
                }
            }

            // queue is full, try to overflow into injector
            if self.push_overflow(task, head, injector) {
                return;
            } else {
                spin_loop_hint();
            }
        }
    }

    /// Migrate half of the queue's tasks into the injector to free up local lots.
    fn push_overflow<P: ThreadParker>(
        &mut self,
        task: &mut Task,
        head: TaskQueueIndex,
        injector: &RawMutex<TaskInjector, P>,
    ) -> bool {
        // the queue was full, try to move half of it to the injector
        // Relaxed orderings since no changes to self.tasks can be observed
        // (as we're the only producers).
        let batch = Self::SIZE / 2;
        if self
            .head()
            .compare_exchange_weak(
                head,
                head.wrapping_add(batch.try_into().unwrap()),
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_err()
        {
            spin_loop_hint();
            return false;
        }

        // form a task list of the stolen tasks, prioritizing them internally.
        let mut batch_list = TaskList::default();
        batch_list.push(task);
        for i in 0..batch {
            let task = self.tasks[(head as usize).wrapping_add(i) % Self::SIZE];
            batch_list.push(unsafe { &mut *task.assume_init() });
        }

        // submit them all as a list to the injector
        injector.lock().push(batch_list);
        true
    }

    /// Dequeue a task from the front of the ring buffer
    pub fn pop<'a>(&mut self) -> Option<&'a mut Task> {
        loop {
            // unsynchronized tail load since only we can change it.
            // Relaxed head load since not observing self.tasks changes as no thread can push.
            let tail = self.unsync_load_tail();
            let head = self.head().load(Ordering::Relaxed);

            // the queue is empty
            if tail.wrapping_sub(head) == 0 {
                return None;
            }

            // try to consume & dequeue the task
            let task = self.tasks[(head as usize) % Self::SIZE];
            match self.head().compare_exchange_weak(
                head,
                head.wrapping_add(1),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(unsafe { &mut *task.assume_init() }),
                Err(_) => spin_loop_hint(),
            }
        }
    }

    /// Dequeue a task from the back of the ring buffer
    pub fn pop_back<'a>(&mut self) -> Option<&'a mut Task> {
        loop {
            // unsynchronized tail load since only we can change it.
            // Relaxed head load since no need to view self.tasks changes to head.
            let tail = self.unsync_load_tail();
            let head = self.head().load(Ordering::Relaxed);

            // the queue is empty
            if tail.wrapping_sub(head) == 0 {
                return None;
            }

            // try to consume & dequeue the task from the tail
            let new_tail = tail.wrapping_sub(1);
            let task = self.tasks[(new_tail as usize) % Self::SIZE];
            match self.pos.compare_exchange_weak(
                Self::to_pos(head, tail),
                Self::to_pos(head, new_tail),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(unsafe { &mut *task.assume_init() }),
                Err(_) => spin_loop_hint(),
            }
        }
    }

    /// Steal a batch of tasks from the `other` into our own queue,
    /// returning the first task stolen if any.
    pub fn steal<'a>(&mut self, other: &Self) -> Option<&'a mut Task> {
        let our_tail = self.unsync_load_tail();
        let our_head = self.head().load(Ordering::Relaxed);
        debug_assert_eq!(
            our_tail.wrapping_sub(our_head),
            0,
            "Should only steal if queue is empty"
        );

        // Acquire loads to other's head & tail to observe other.task's writes.
        loop {
            let tail = other.tail().load(Ordering::Acquire);
            let head = other.head().load(Ordering::Acquire);
            let size = tail.wrapping_sub(head);
            let mut batch = size - (size / 2);

            // other's queue is empty
            if batch == 0 {
                return None;
            }

            // read a batch (half) of other's tasks into ours
            for i in 0..batch {
                let task = other.tasks[(head.wrapping_add(i) as usize) % Self::SIZE];
                self.tasks[(our_tail.wrapping_add(i) as usize) % Self::SIZE] = task;
            }

            // try to commit the steal, use Relaxed since CAS acts as a release.
            if other
                .head()
                .compare_exchange_weak(
                    head,
                    head.wrapping_add(batch),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_err()
            {
                spin_loop_hint();
                continue;
            }

            // make our stolen tasks available & return the last one
            batch -= 1;
            if batch != 0 {
                self.tail()
                    .store(our_tail.wrapping_add(batch), Ordering::Release);
            }
            let task = self.tasks[(our_tail.wrapping_add(batch) as usize) % Self::SIZE];
            return Some(unsafe { &mut *task.assume_init() });
        }
    }
}
