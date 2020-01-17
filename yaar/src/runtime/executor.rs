use super::{
    task::{CachedFutureTask, List as TaskList, Priority, Task},
    Platform,
};
use core::{
    cell::Cell,
    future::Future,
    mem::{self, MaybeUninit},
    num::NonZeroUsize,
    ptr::NonNull,
};
use yaar_lock::sync::{RawMutex, ThreadParker};

pub struct Config<'p, 'w, P> {
    pub platform: &'p P,
    pub workers: &'w [Worker],
    pub max_threads: NonZeroUsize,
}

impl<'p, 'w, P> Config<'p, 'w, P>
where
    P: Platform,
    P::Parker: ThreadParker,
{
    pub fn run<T>(&mut self, future: impl Future<Output = T>) -> T {
        // run the executor normally
        if self.workers.len() != 0 {
            Executor::run(self, future)

        // if no workers provided, allocate one on the stack
        } else {
            let mut worker = MaybeUninit::<Worker>::uninit();
            let old_workers = mem::replace(&mut self.workers, unsafe {
                core::slice::from_raw_parts(worker.as_mut_ptr(), 1)
            });
            let result = Executor::run(self, future);
            self.workers = old_workers;
            result
        }
    }
}

pub(super) static EXECUTOR_CELL: ExecutorCell = ExecutorCell(Cell::new(None));

pub(super) struct ExecutorCell(pub Cell<Option<ExecutorRef>>);

unsafe impl Sync for ExecutorCell {}

pub(super) struct ExecutorRef {
    ptr: *const (),
    _schedule: fn(*const (), task: *mut Task),
}

impl ExecutorRef {
    pub fn schedule(&self, task: *mut Task) {
        (self._schedule)(self.ptr, task)
    }
}

struct Executor<'p, 'w, P>
where
    P: Platform,
    P::Parker: ThreadParker,
{
    platform: &'p P,
    workers: &'w [Worker],
    stop_signal: P::Parker,
    runtime: RawMutex<Runtime, P::Parker>,
}

struct Runtime {
    run_queue: TaskList,
    runq_size: usize,
    free_threads: usize,
    max_threads: NonZeroUsize,
    idle_threads: Option<NonNull<Thread>>,
    idle_workers: Option<NonNull<Worker>>,
}

impl<'p, 'w, P> Executor<'p, 'w, P>
where
    P: Platform,
    P::Parker: ThreadParker,
{
    pub fn run<T>(config: &Config<'p, 'w, P>, future: impl Future<Output = T>) -> T {
        let executor = Self {
            platform: config.platform,
            workers: config.workers,
            stop_signal: P::Parker::default(),
            runtime: RawMutex::new(Runtime {
                run_queue: TaskList::default(),
                runq_size: 0,
                free_threads: config.max_threads.get(),
                max_threads: config.max_threads,
                idle_threads: None,
                idle_workers: None,
            }),
        };

        EXECUTOR_CELL.0.set(Some(ExecutorRef {
            ptr: &executor as *const Self as *const (),
            _schedule: |ptr, task| unsafe { (&*(ptr as *const Self)).schedule(&mut *task) },
        }));

        let mut future = CachedFutureTask::new(Priority::Normal, future);
        let _ = future.resume();

        EXECUTOR_CELL.0.set(None);
        future.into_inner().unwrap()
    }

    fn schedule(&self, task: &mut Task) {}
}

struct Thread {
    next: Option<NonNull<Self>>,
}

pub struct Worker {
    next: Option<NonNull<Self>>,
}
