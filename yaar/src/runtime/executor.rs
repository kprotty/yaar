use super::{CachedFutureTask, Platform, Task, TaskInjector, TaskList, TaskPriority, TaskQueue};
use core::{
    cell::{Cell, RefCell},
    future::Future,
    mem::{drop, MaybeUninit},
    num::{NonZeroU64, NonZeroUsize},
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
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
