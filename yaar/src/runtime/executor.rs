

pub fn run<T, P>(
    platform: &P,
    workers: &mut [Worker],
    max_threads: NonZeroUsize,
    future: impl Future<Output = T>,
) -> T 
where
    P: Platform,
    P::Parker: ThreadParker,
{
    // Allocate a worker on stack if none are provided
    if workers.len() == 0 {
        let mut worker = MaybeUninit::<Worker>::uninit();
        let workers = unsafe { from_raw_parts_mut(worker.as_mut_ptr(), 1) };
        Executor::run(platform, workers, max_threads, future)
    } else {
        Executor::run(platform, workers, max_threads, future)
    }
}

pub(super) static EXECUTOR_CELL: ExecutorCell = ExecutorCell(Cell::new(None));

pub(super) struct ExecutorCell(Cell<Option<ExecutorRef>>);

unsafe impl Sync for ExecutorCell {}

pub(super) struct ExecutorRef {
    ptr: *const (),
    _schedule: fn(*const (), *mut Task),
}

struct Executor<'p, 'w, P>
where
    P: Platform,
    P::Parker: ThreadParker,
{
    platform: &'p P,
    workers: &'w mut [Worker],
    stop_signal: P::Parker,
    workers_idle: AtomicUsize,
    workers_stealing: AtomicUsize,
    injector: RawMutex<Injector, P::Parker>,
    thread_pool: RawMutex<ThreadPool, P::Parker>,
}

impl<'p, 'w, P> Executor<'p, 'w, P>
where
    P: Platform,
    P::Parker: ThreadParker,
{
    pub fn run<T>(
        platform: &P,
        workers: &mut [Worker],
        max_threads: NonZeroUsize,
        future: impl Future<Output = T>,
    ) -> T {
        let executor = Self {
            platform,
            workers,
            stop_signal: P::Parker::default(),
            workers_idle: AtomicUsize::new(0),
            workers_stealing: AtomicUsize::new(0),
            injector: RawMutex::new(Injector {
                list: TaskList::default(),
                size: 0,
            }),
            thread_pool: RawMutex::new(ThreadPool {
                max_threads: max_threads.get(),
                free_threads: max_threads.get(),
                idle_threads: None,
                idle_workers: None,
            }),
        };
        
        // set the current global executor
        EXECUTOR_CELL.0.set(ExecutorRef {
            ptr: &executor as *const Self as *const (),
            _schedule: |ptr, task| { unsafe { (&*(ptr as *const Self)).schedule(&mut *task) } },
        });
        
        // schedule the future onto the executor
        let mut future = CachedFutureTask::new(Priority::Normal, future);
        executor.schedule(future.as_mut());
        
        // get an idle worker and use the main thread to run it
        Thread::run({
            let mut thread_pool = executor.thread_pool.lock();
            for worker in executor.workers.iter_mut() {
                *worker = Worker::new(&executor);
                thread_pool.put_worker(&executor.workers_idle, worker);
            }
            thread_pool.find_worker(&executor.workers_idle).unwrap()
        });
        
        // wait for the executor to be signaled, remove the global ref & return the future result.
        executor.stop_signal.park();
        EXECUTOR_CELL.0.set(None);
        future.into_inner().unwrap()
    }
}

struct ThreadPool {
    max_threads: usize,
    free_threads: usize,
    idle_threads: Option<NonNull<Thread>>,
    idle_workers: Option<NonNull<Worker>>,
}

impl ThreadPool {
    pub fn put_thread(&mut self, thread: &mut Thread) {
        thread.next = self.idle_threads;
        self.idle_threads = thread;
    }

    pub fn put_worker(&mut self, workers_idle: &AtomicUsize, worker: &mut Worker) {
        worker.next = self.idle_workers;
        self.idle_workers = NonNull::new(worker);
        workers_idle.fetch_add(1, Ordreing::Release);
    }

    pub fn find_worker<'a>(&mut self, workers_idle: &AtomicUsize) -> Option<&'a mut Worker> {
        self.idle_workers.map(|worker| {
            let worker = unsafe { &mut *worker.as_mut_ptr() };
            self.idle_workers = worker.next;
            workers_idle.fetch_sub(1, Ordering::Release);
            worker
        })
    }

    pub fn spawn_worker<P: Platform>(&mut self, workers_idle: &AtomicUsize, platform: &P) -> bool {
        self.find_worker(workers_idle)
            .and_then(|worker| {
                // try to pop a thread from the idle threads list
                if let Some(thread) = self.idle_threads {
                    let thread = unsafe { &mut *thread.as_mut_ptr() };
                    self.idle_threads = thread.next;
                    thread.worker = Some(worker);
                    thread.signal.unpark();
                    return Some(());
                }
                
                // try to create a new thread
                if self.free_threads != 0 {
                    let worker = worker as *mut _ as usize;
                    if platform.spawn_thread(worker, Thread::<P>::run) {
                        self.free_threads -= 1;
                        return Some(());
                    }
                }

                // failed to spawn worker thread, restore the idle Worker
                self.put_worker(workers_idle);
                None
            })
            .is_some()
    }
}

struct Thread<P>
where
    P: Platform,
    P::Parker: ThreadParker,
{
    next: Option<NonNull<Self>>,
    worker: Option<NonNull<Worker>>,
    signal: P::Parker,
    is_stealing: bool,
}

type StaticExecutor<P> = Executor<'static, 'static, P>;

impl<P> Thread<P> {
    pub extern "system" fn run(worker: usize) {
        let (worker, executor) = unsafe {
            let worker = &mut *(worker as *mut Worker);
            let executor = &*(worker.executor as StaticExecutor<P>);
            (worker, executor)
        };

        let mut thread = Self {
            next: None,
            worker: NonNull::new(worker),
            signal: P::Parker::default(),
            is_stealing: executor.workers_stealing.load(Ordering::Relaxed) != 0,
        };
        
        let mut tick = 0;
        while let Some(task) = thread.poll(executor, tick) {
            unsafe { task.resume() };
            tick += 1;
        }

        let thread_pool = executor.thread_pool.lock();
        thread_pool.free_threads += 1;
        if thread_pool.free_threads == thread_pool.max_threads.get() {
            executor.stop_signal.unpark();
        }
    }

    fn poll<'a>(&mut self, executor: &Executor<'static, 'static, P>, tick: usize) -> Option<&'a mut Task> {
        'poll: loop {
            let wait_time = {
                let worker = match self.worker {
                    Some(worker) => unsafe { &mut *worker.as_mut_ptr() },
                    None => return None,
                };

                let mut wait_time = None;
                if let Some(task) = poll_timers(worker, executor, &mut wait_time) {
                    return Some(task);
                }

                if let Some(task) = poll_local(worker, tick) {
                    return Some(task);
                }

                if let Some(task) = pool_global(executor, worker, 0) {
                    return Some(task);
                }

                if let Some(task) = poll_io(executor, None) {
                    return Some(task);
                }

                if let Some(task) = poll_workers(executor, worker) {
                    return Some(task);
                }

                if let Some(task) = poll_global(executor, worker, !0) {
                    return Some(task);
                }

                let thread_pool = executor.thread_pool.lock();
                thread_pool.put_worker(worker);
                self.worker = None;
                wait_time
            };

            let was_stealing = self.is_stealing;
            if was_stealing {
                self.is_stealing = false;
                executor.workers_stealing.fetch_sub(Ordering::Release);
            }

            for worker in executor.workers.iter() {
                if !worker.has_pending_tasks() {
                    continue;
                }
                match executor
                    .thread_pool
                    .lock()
                    .find_worker(&executor.workers_idle)
                {
                    None => break,
                    Some(worker) => {
                        self.worker = worker;
                        if was_stealing {
                            self.is_stealing = true;
                            executor.workers_stealing.fetch_add(Ordering::Release);
                        }
                        continue 'poll;
                    }
                }
            }

            if let Some(task) = poll_io(executor, wait_time) {
                self.worker = executor
                    .thread_pool
                    .lock()
                    .find_worker(&executor.workers_idle)
                    .unwrap();
                if was_stealing {
                    self.is_stealing = true;
                    executor.workers_stealing.fetch_add(Ordering::Release);
                }
                return Some(task);
            }

            executor
                .thread_pool
                .lock()
                .put_thread(self);
            self.signal.park();
            self.signal.reset();
        }    
    }

    
}

pub struct Worker {
    next: Option<NonNull<Self>>,
    executor: usize,
    run_queue: LocalQueue,
}

impl Worker {
    pub(super) fn new<E>(executor: &E) -> Self {
        Self {
            next: None,
            executor: executor as *const _ as usize,
            run_queue: LocalQueue::default(),
        }
    }

    pub(super) fn has_pending_tasks(&self) -> bool {
        // TODO: check timers too
        self.run_queue.size() != 0
    }
}