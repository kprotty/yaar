use super::{CachedFutureTask, Platform, Task, TaskPriority};
use core::{cell::Cell, future::Future, mem::MaybeUninit, num::NonZeroUsize, ptr::NonNull};
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
            // TODO: event loop
            future
                .into_inner()
                .expect("The provided future hit a deadlock")
        };

        // store what was originally in the executor cell stack & ensure our context was popped.
        let our_ref = EXECUTOR_CELL.0.replace(old_ref);
        debug_assert_eq!(our_ref, ref_ptr);
        result
    }

    fn schedule(&self, task: &mut Task) {}
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

struct Thread<'a, P>
where
    P: Platform,
    P::Parker: ThreadParker,
{
    next: Cell<Option<&'a Self>>,
    worker: Cell<Option<&'a Worker>>,
    parker: P::Parker,
}

#[derive(Default)]
pub struct Worker {
    next: usize,
}
