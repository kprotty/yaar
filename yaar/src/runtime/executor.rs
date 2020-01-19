use yaar_lock::sync::{RawMutex, ThreadParker};
use super::{Platform, Task};
use core::{
    num::NonZeroUsize,
    future::Future,
    mem::MaybeUninit,
    cell::Cell,
    ptr::NonNull,
};

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
        let workers = core::slice::from_raw(&worker);
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
    ptr: *const (),
    schedule: fn(*const (), *mut Task),
}

pub(super) fn with_executor_ref<T>(scoped: impl FnOnce(&ExecutorRef) -> T) -> T {
    scoped(unsafe {
        &*EXECUTOR_CELL.0.get()
            .unwrap("Attempted to get the current running executor without one running")
            .as_ptr()
    })
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

        unsafe { MaybeUninit::zeroed().assume_init() }
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