#![forbid(unsafe_code)]

use crossbeam_deque::{Injector, Steal, Stealer as QueueStealer, Worker as QueueWorker};
use gcd::Gcd;
use parking_lot::{Condvar, Mutex};
use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::Future,
    hint::spin_loop,
    mem::{drop, replace, size_of},
    num::NonZeroUsize,
    panic::{catch_unwind, resume_unwind, AssertUnwindSafe},
    pin::Pin,
    rc::Rc,
    sync::atomic::{fence, AtomicU8, AtomicUsize, Ordering},
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
    thread,
};
use try_lock::TryLock;

#[derive(Default)]
pub struct Builder {
    worker_threads: Option<NonZeroUsize>,
}

impl Builder {
    pub const fn new() -> Self {
        Self {
            worker_threads: None,
        }
    }

    pub fn worker_threads(&mut self, worker_threads: usize) -> &mut Self {
        self.worker_threads = NonZeroUsize::new(worker_threads);
        self
    }

    pub fn block_on<F>(&self, future: F) -> F::Output
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let executor = Arc::new(Executor::new(self.worker_threads));
        let mut join_handle = Task::spawn(future, &executor, None);

        join_handle
            .joinable
            .take()
            .expect("block_on future wasn't polled to completion")
            .join()
    }
}

pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    Thread::try_with(|thread| Task::spawn(future, &thread.executor, Some(thread)))
        .expect("spawn() called outside of the executor")
}

struct TaskWaker {
    state: AtomicU8,
    waker: TryLock<Option<Waker>>,
}

impl TaskWaker {
    const EMPTY: u8 = 0;
    const UPDATING: u8 = 1;
    const READY: u8 = 2;
    const NOTIFIED: u8 = 3;

    fn new() -> Self {
        Self {
            state: AtomicU8::new(Self::EMPTY),
            waker: TryLock::new(None),
        }
    }

    fn poll(&self, ctx: &mut Context<'_>) -> Poll<()> {
        let state = self.state.load(Ordering::Acquire);
        match state {
            Self::EMPTY | Self::READY => {}
            Self::NOTIFIED => return Poll::Ready(()),
            _ => unreachable!(),
        }

        match self.state.compare_exchange(
            state,
            Self::UPDATING,
            Ordering::Acquire,
            Ordering::Acquire,
        ) {
            Ok(_) => {}
            Err(Self::NOTIFIED) => return Poll::Ready(()),
            Err(_) => unreachable!(),
        }

        let mut waker = self.waker.try_lock().unwrap();
        let will_wake = waker
            .as_ref()
            .map(|waker| ctx.waker().will_wake(waker))
            .unwrap_or(false);

        if !will_wake {
            *waker = Some(ctx.waker().clone());
        }

        drop(waker);
        match self.state.compare_exchange(
            Self::UPDATING,
            Self::READY,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Poll::Pending,
            Err(Self::NOTIFIED) => Poll::Ready(()),
            _ => unreachable!(),
        }
    }

    fn wake(&self) {
        if self.state.swap(Self::NOTIFIED, Ordering::AcqRel) == Self::READY {
            let waker = self.waker.try_lock().unwrap().take();
            waker.unwrap().wake();
        }
    }
}

struct TaskState {
    state: AtomicU8,
}

impl TaskState {
    const IDLE: u8 = 0;
    const SCHEDULED: u8 = 1;
    const RUNNING: u8 = 2;
    const NOTIFIED: u8 = 3;

    fn new() -> Self {
        Self {
            state: AtomicU8::new(Self::IDLE),
        }
    }

    fn transition_to_scheduled(&self) -> bool {
        self.state
            .fetch_update(Ordering::Release, Ordering::Relaxed, |state| match state {
                Self::IDLE => Some(Self::SCHEDULED),
                Self::RUNNING => Some(Self::NOTIFIED),
                _ => None,
            })
            .map(|state| state == Self::IDLE)
            .unwrap_or(false)
    }

    fn transition_to_running(&self) {
        assert_eq!(self.state.load(Ordering::Relaxed), Self::SCHEDULED);
        self.state.store(Self::RUNNING, Ordering::Relaxed);
    }

    fn transition_to_idle(&self) -> bool {
        match self.state.compare_exchange(
            Self::RUNNING,
            Self::IDLE,
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            Ok(_) => true,
            Err(state) => {
                assert_eq!(state, Self::NOTIFIED);
                fence(Ordering::Acquire);
                self.state.store(Self::RUNNING, Ordering::Relaxed);
                false
            }
        }
    }
}

enum TaskData<F: Future> {
    Pending(Pin<Box<F>>),
    Polling,
    Ready(Result<F::Output, Box<dyn Any + Send + 'static>>),
    Joined,
}

struct Task<F: Future> {
    state: TaskState,
    waker: TaskWaker,
    data: TryLock<TaskData<F>>,
    executor: Arc<Executor>,
}

impl<F> Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn spawn(
        future: F,
        executor: &Arc<Executor>,
        thread: Option<&Thread>,
    ) -> JoinHandle<F::Output> {
        let task = Arc::new(Self {
            state: TaskState::new(),
            waker: TaskWaker::new(),
            data: TryLock::new(TaskData::Pending(Box::pin(future))),
            executor: executor.clone(),
        });

        executor.task_begin();
        executor.schedule(task.clone(), thread, false);

        JoinHandle {
            joinable: Some(task),
        }
    }

    fn schedule(self: Arc<Self>) {
        let mut task = Some(self);
        let _ = Thread::try_with(|thread| {
            let task = task.take().unwrap();
            thread.executor.schedule(task, Some(thread), false);
        });

        if let Some(task) = task.take() {
            let executor = task.executor.clone();
            executor.schedule(task, None, false);
        }
    }
}

impl<F> Wake for Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn wake(self: Arc<Self>) {
        if self.state.transition_to_scheduled() {
            self.schedule();
        }
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if self.state.transition_to_scheduled() {
            self.clone().schedule();
        }
    }
}

trait TaskRunnable: Send + Sync {
    fn run(self: Arc<Self>, thread: &Thread);
}

impl<F> TaskRunnable for Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn run(self: Arc<Self>, thread: &Thread) {
        self.state.transition_to_running();

        let mut data = self.data.try_lock().unwrap();
        let mut future = match replace(&mut *data, TaskData::Polling) {
            TaskData::Pending(future) => future,
            _ => unreachable!(),
        };

        let poll_result = catch_unwind(AssertUnwindSafe(|| {
            let waker = Waker::from(self.clone());
            let mut ctx = Context::from_waker(&waker);
            future.as_mut().poll(&mut ctx)
        }));

        let result = match poll_result {
            Err(error) => Err(error),
            Ok(Poll::Ready(result)) => Ok(result),
            Ok(Poll::Pending) => {
                *data = TaskData::Pending(future);
                drop(data);

                if !self.state.transition_to_idle() {
                    thread.executor.schedule(self, Some(thread), true);
                }

                return;
            }
        };

        *data = TaskData::Ready(result);
        drop(data);

        self.waker.wake();
        thread.executor.task_complete();
    }
}

trait TaskJoinable<T> {
    fn poll_join(&self, ctx: &mut Context<'_>) -> Poll<T>;
    fn join(&self) -> T;
}

impl<F: Future> TaskJoinable<F::Output> for Task<F> {
    fn poll_join(&self, ctx: &mut Context<'_>) -> Poll<F::Output> {
        match self.waker.poll(ctx) {
            Poll::Ready(_) => Poll::Ready(self.join()),
            Poll::Pending => Poll::Pending,
        }
    }

    fn join(&self) -> F::Output {
        match replace(&mut *self.data.try_lock().unwrap(), TaskData::Joined) {
            TaskData::Ready(Ok(result)) => result,
            TaskData::Ready(Err(error)) => resume_unwind(error),
            _ => unreachable!(),
        }
    }
}

pub struct JoinHandle<T> {
    joinable: Option<Arc<dyn TaskJoinable<T> + Send + Sync>>,
}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let joinable = self
            .joinable
            .as_ref()
            .expect("JoinHandle polled after completion");

        if let Poll::Ready(result) = joinable.poll_join(ctx) {
            self.joinable = None;
            return Poll::Ready(result);
        }

        Poll::Pending
    }
}

struct Producer {
    be_fair: Cell<bool>,
    worker: QueueWorker<Arc<dyn TaskRunnable>>,
    stealer: QueueStealer<Arc<dyn TaskRunnable>>,
}

impl Default for Producer {
    fn default() -> Self {
        let worker = QueueWorker::new_lifo();
        let stealer = worker.stealer();

        Self {
            be_fair: Cell::new(false),
            worker,
            stealer,
        }
    }
}

impl Producer {
    fn push(&self, runnable: Arc<dyn TaskRunnable>, be_fair: bool) {
        self.worker.push(runnable);
        if be_fair {
            self.be_fair.set(true);
        }
    }

    fn pop(&self, be_fair: bool) -> Option<Arc<dyn TaskRunnable>> {
        let be_fair = be_fair || self.be_fair.replace(false);
        if !be_fair {
            return self.worker.pop();
        }

        loop {
            match self.stealer.steal() {
                Steal::Success(runnable) => return Some(runnable),
                Steal::Retry => spin_loop(),
                Steal::Empty => return None,
            }
        }
    }

    fn consume(&self, executor: &Executor) -> Steal<Arc<dyn TaskRunnable>> {
        executor.injector.steal_batch_and_pop(&self.worker)
    }

    fn steal(&self, worker: &Worker) -> Steal<Arc<dyn TaskRunnable>> {
        worker.stealer.steal_batch_and_pop(&self.worker)
    }
}

struct Worker {
    idle_next: AtomicUsize,
    stealer: QueueStealer<Arc<dyn TaskRunnable>>,
    producer: TryLock<Option<Producer>>,
}

impl Default for Worker {
    fn default() -> Self {
        let producer = Producer::default();
        let stealer = producer.stealer.clone();

        Self {
            idle_next: AtomicUsize::new(0),
            stealer,
            producer: TryLock::new(Some(producer)),
        }
    }
}

struct Executor {
    idle: AtomicUsize,
    tasks: AtomicUsize,
    searching: AtomicUsize,
    workers: Box<[Worker]>,
    thread_pool: ThreadPool,
    rng_iter_source: RandomIterSource,
    injector: Injector<Arc<dyn TaskRunnable>>,
}

impl Executor {
    fn new(worker_threads: Option<NonZeroUsize>) -> Self {
        let worker_threads = worker_threads
            .or_else(|| NonZeroUsize::new(num_cpus::get()))
            .or(NonZeroUsize::new(1))
            .unwrap();

        let executor = Self {
            idle: AtomicUsize::new(0),
            tasks: AtomicUsize::new(0),
            searching: AtomicUsize::new(0),
            workers: (0..worker_threads.get())
                .map(|_| Worker::default())
                .collect(),
            thread_pool: ThreadPool::new(worker_threads),
            rng_iter_source: RandomIterSource::from(worker_threads),
            injector: Injector::new(),
        };

        for worker_index in (0..executor.workers.len()).rev() {
            executor.push_idle_worker(worker_index);
        }

        executor
    }

    fn schedule(
        self: &Arc<Self>,
        runnable: Arc<dyn TaskRunnable>,
        thread: Option<&Thread>,
        be_fair: bool,
    ) {
        if let Some(thread) = thread {
            if let Some(producer) = thread.producer.borrow().as_ref() {
                producer.push(runnable, be_fair);
                self.notify();
                return;
            }
        }

        self.injector.push(runnable);
        fence(Ordering::SeqCst);
        self.notify();
    }

    fn notify(self: &Arc<Self>) {
        if self.peek_idle_worker().is_none() {
            return;
        }

        let searching = self.searching.load(Ordering::Relaxed);
        assert!(searching <= self.workers.len());

        if searching > 0 {
            return;
        }

        if let Err(searching) =
            self.searching
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::Relaxed)
        {
            assert!(searching <= self.workers.len());
            return;
        }

        if let Some(worker_index) = self.pop_idle_worker() {
            match self.thread_pool.spawn(self, worker_index) {
                Ok(_) => return,
                Err(_) => self.push_idle_worker(worker_index),
            }
        }

        let searching = self.searching.fetch_sub(1, Ordering::Relaxed);
        assert!(searching <= self.workers.len());
        assert_ne!(searching, 0);
    }

    fn task_begin(&self) {
        let tasks = self.tasks.fetch_add(1, Ordering::Relaxed);
        assert_ne!(tasks, usize::MAX);
    }

    fn task_complete(&self) {
        let tasks = self.tasks.fetch_sub(1, Ordering::Release);
        assert_ne!(tasks, 0);

        if tasks == 1 {
            self.thread_pool.shutdown();
        }
    }

    fn search_begin(&self) -> bool {
        let searching = self.searching.load(Ordering::Relaxed);
        assert!(searching <= self.workers.len());

        if (2 * searching) >= self.workers.len() {
            return false;
        }

        let searching = self.searching.fetch_add(1, Ordering::Relaxed);
        assert!(searching < self.workers.len());
        true
    }

    fn search_discovered(self: &Arc<Self>) {
        let searching = self.searching.fetch_sub(1, Ordering::SeqCst);
        assert!(searching <= self.workers.len());
        assert_ne!(searching, 0);

        if searching == 1 {
            self.notify();
        }
    }

    fn search_failed(&self, worker_index: usize, was_searching: bool) -> bool {
        assert!(worker_index <= self.workers.len());
        self.push_idle_worker(worker_index);

        was_searching && {
            let searching = self.searching.fetch_sub(1, Ordering::SeqCst);
            assert!(searching <= self.workers.len());
            assert_ne!(searching, 0);

            searching == 1 && !self.injector.is_empty()
        }
    }

    fn search_retry(&self) -> Option<usize> {
        self.pop_idle_worker().map(|worker_index| {
            let searching = self.searching.fetch_add(1, Ordering::Relaxed);
            assert!(searching < self.workers.len());
            worker_index
        })
    }

    const IDLE_BITS: u32 = usize::BITS / 2;
    const IDLE_MASK: usize = (1 << Self::IDLE_BITS) - 1;

    fn push_idle_worker(&self, worker_index: usize) {
        assert!(worker_index <= self.workers.len());
        assert!(worker_index < Self::IDLE_MASK);

        let _ = self
            .idle
            .fetch_update(Ordering::Release, Ordering::Relaxed, |idle| {
                let top_index = idle & Self::IDLE_MASK;
                self.workers[worker_index]
                    .idle_next
                    .store(top_index, Ordering::Relaxed);

                let aba_count = (idle >> Self::IDLE_BITS) + 1;
                Some((aba_count << Self::IDLE_BITS) | (worker_index + 1))
            });
    }

    fn peek_idle_worker(&self) -> Option<usize> {
        let idle = self.idle.load(Ordering::SeqCst);
        (idle & Self::IDLE_MASK).checked_sub(1)
    }

    fn pop_idle_worker(&self) -> Option<usize> {
        self.idle
            .fetch_update(Ordering::Release, Ordering::Relaxed, |idle| {
                let worker_index = (idle & Self::IDLE_MASK).checked_sub(1)?;
                fence(Ordering::Acquire);
                let next_index = self.workers[worker_index].idle_next.load(Ordering::Relaxed);
                Some((idle & !Self::IDLE_MASK) | next_index)
            })
            .map(|idle| (idle & Self::IDLE_MASK) - 1)
            .ok()
    }
}

struct Pool {
    idle: usize,
    spawned: usize,
    shutdown: bool,
    notified: VecDeque<usize>,
}

struct ThreadPool {
    max_threads: NonZeroUsize,
    condvar: Condvar,
    pool: Mutex<Pool>,
}

impl ThreadPool {
    fn new(max_threads: NonZeroUsize) -> Self {
        Self {
            max_threads,
            condvar: Condvar::new(),
            pool: Mutex::new(Pool {
                idle: 0,
                spawned: 0,
                shutdown: false,
                notified: VecDeque::with_capacity(max_threads.get()),
            }),
        }
    }

    fn spawn(&self, executor: &Arc<Executor>, worker_index: usize) -> Result<(), ()> {
        let mut pool = self.pool.lock();
        if pool.shutdown {
            return Err(());
        }

        if pool.notified.len() < pool.idle {
            pool.notified.push_back(worker_index);
            self.condvar.notify_one();
            return Ok(());
        }

        assert!(pool.spawned <= self.max_threads.get());
        if pool.spawned == self.max_threads.get() {
            return Err(());
        }

        let position = pool.spawned;
        pool.spawned += 1;
        drop(pool);

        if position == 0 {
            Self::run(executor, worker_index, position);
            return Ok(());
        }

        let executor = executor.clone();
        thread::Builder::new()
            .spawn(move || Self::run(&executor, worker_index, position))
            .map(drop)
            .map_err(|_| self.finish())
    }

    fn run(executor: &Arc<Executor>, worker_index: usize, position: usize) {
        Thread::run(executor, worker_index, position);
        executor.thread_pool.finish();
    }

    fn finish(&self) {
        let mut pool = self.pool.lock();
        assert!(pool.spawned <= self.max_threads.get());
        assert_ne!(pool.spawned, 0);
        pool.spawned -= 1;
    }

    fn wait(&self) -> Result<usize, ()> {
        let mut pool = self.pool.lock();
        loop {
            if pool.shutdown {
                return Err(());
            }

            if let Some(worker_index) = pool.notified.pop_back() {
                return Ok(worker_index);
            }

            pool.idle += 1;
            assert!(pool.idle <= self.max_threads.get());

            self.condvar.wait(&mut pool);

            assert!(pool.idle <= self.max_threads.get());
            assert_ne!(pool.idle, 0);
            pool.idle -= 1;
        }
    }

    fn shutdown(&self) {
        let mut pool = self.pool.lock();
        if replace(&mut pool.shutdown, true) {
            return;
        }

        if pool.idle > 0 && pool.notified.len() < pool.idle {
            self.condvar.notify_all();
        }
    }
}

struct Thread {
    executor: Arc<Executor>,
    producer: RefCell<Option<Producer>>,
}

impl Thread {
    fn with_tls<F>(f: impl FnOnce(&mut Option<Rc<Self>>) -> F) -> F {
        thread_local!(static TLS: RefCell<Option<Rc<Thread>>> = RefCell::new(None));
        TLS.with(|ref_cell| f(&mut *ref_cell.borrow_mut()))
    }

    fn try_with<F>(f: impl FnOnce(&Self) -> F) -> Option<F> {
        Self::with_tls(|tls| tls.as_ref().map(Rc::clone)).map(|rc| f(&*rc))
    }

    fn run(executor: &Arc<Executor>, worker_index: usize, position: usize) {
        let thread = Rc::new(Self {
            executor: executor.clone(),
            producer: RefCell::new(None),
        });

        let old_tls = Self::with_tls(|tls| replace(tls, Some(thread.clone())));
        ThreadRef::run(&*thread, worker_index, position);
        Self::with_tls(|tls| *tls = old_tls);
    }
}

struct ThreadRef<'a> {
    thread: &'a Thread,
    tick: usize,
    searching: bool,
    rng: RandomGenerator,
    worker_index: Option<usize>,
}

impl<'a> ThreadRef<'a> {
    fn run(thread: &'a Thread, worker_index: usize, position: usize) {
        let mut rng = RandomGenerator::from(position);
        let tick = rng.gen();

        let mut thread_ref = Self {
            thread,
            tick,
            searching: false,
            rng,
            worker_index: None,
        };

        thread_ref.transition_to_running(worker_index);
        thread_ref.run_until_shutdown();
    }

    fn transition_to_running(&mut self, worker_index: usize) {
        assert!(self.worker_index.is_none());
        self.worker_index = Some(worker_index);

        assert!(!self.searching);
        self.searching = true;

        let producer = self.thread.executor.workers[worker_index]
            .producer
            .try_lock()
            .unwrap()
            .take()
            .unwrap();

        self.thread
            .producer
            .replace(Some(producer))
            .map(|_| unreachable!());
    }

    fn transition_to_idle(&mut self, worker_index: usize) -> bool {
        let producer = self.thread.producer.take().unwrap();

        self.thread.executor.workers[worker_index]
            .producer
            .try_lock()
            .unwrap()
            .replace(producer)
            .map(|_| unreachable!());

        assert_eq!(self.worker_index, Some(worker_index));
        self.worker_index = None;

        let was_searching = replace(&mut self.searching, false);
        self.thread
            .executor
            .search_failed(worker_index, was_searching)
    }

    fn run_until_shutdown(&mut self) {
        while let Some(runnable) = self.poll() {
            if replace(&mut self.searching, false) {
                self.thread.executor.search_discovered();
            }

            self.tick = self.tick.wrapping_add(1);
            runnable.run(self.thread);
        }
    }

    fn poll(&mut self) -> Option<Arc<dyn TaskRunnable>> {
        loop {
            if let Some(worker_index) = self.worker_index {
                if let Some(runnable) = self.pop(worker_index) {
                    return Some(runnable);
                }

                if self.transition_to_idle(worker_index) {
                    if let Some(worker_index) = self.thread.executor.search_retry() {
                        self.transition_to_running(worker_index);
                        continue;
                    }
                }
            }

            match self.thread.executor.thread_pool.wait() {
                Ok(worker_index) => self.transition_to_running(worker_index),
                Err(()) => return None,
            }
        }
    }

    fn pop(&mut self, worker_index: usize) -> Option<Arc<dyn TaskRunnable>> {
        let producer = self.thread.producer.borrow();
        let producer = producer.as_ref().unwrap();
        let executor = &self.thread.executor;

        let be_fair = self.tick % 61 == 0;
        if be_fair {
            if let Some(runnable) = producer.consume(&*executor).success() {
                return Some(runnable);
            }
        }

        if let Some(runnable) = producer.pop(be_fair) {
            return Some(runnable);
        }

        self.searching = self.searching || executor.search_begin();
        if self.searching {
            for _retries in 0..32 {
                let mut was_contended = match producer.consume(&*executor) {
                    Steal::Success(runnable) => return Some(runnable),
                    Steal::Empty => false,
                    Steal::Retry => true,
                };

                for steal_index in self.rng.gen_iter(executor.rng_iter_source) {
                    if steal_index == worker_index {
                        continue;
                    }

                    match producer.steal(&executor.workers[steal_index]) {
                        Steal::Success(runnable) => return Some(runnable),
                        Steal::Retry => was_contended = true,
                        Steal::Empty => {}
                    }
                }

                if was_contended {
                    spin_loop();
                } else {
                    break;
                }
            }
        }

        None
    }
}

#[derive(Copy, Clone)]
struct RandomIterSource {
    range: NonZeroUsize,
    prime: NonZeroUsize,
}

impl From<NonZeroUsize> for RandomIterSource {
    fn from(range: NonZeroUsize) -> Self {
        Self {
            range,
            prime: ((range.get() / 2)..range.get())
                .rev()
                .map(|prime| prime.gcd(range.get()))
                .filter_map(NonZeroUsize::new)
                .next()
                .unwrap(),
        }
    }
}

struct RandomGenerator {
    xorshift: NonZeroUsize,
}

impl From<usize> for RandomGenerator {
    fn from(seed: usize) -> Self {
        #[cfg(target_pointer_width = "64")]
        const HASH: usize = 0x9E3779B97F4A7C15;

        #[cfg(target_pointer_width = "32")]
        const HASH: usize = 0x9E3779B9;

        Self {
            xorshift: NonZeroUsize::new(seed.wrapping_mul(HASH))
                .or(NonZeroUsize::new(0xdeadbeef))
                .unwrap(),
        }
    }
}

impl RandomGenerator {
    fn gen(&mut self) -> usize {
        let shifts = match size_of::<usize>() {
            8 => (13, 7, 17),
            4 => (13, 17, 5),
            _ => unreachable!(),
        };

        let mut xs = self.xorshift.get();
        xs ^= xs << shifts.0;
        xs ^= xs >> shifts.1;
        xs ^= xs << shifts.2;

        self.xorshift = NonZeroUsize::new(xs).unwrap();
        xs
    }

    fn gen_iter(&mut self, iter_source: RandomIterSource) -> impl Iterator<Item = usize> {
        let mut offset = self.gen();

        (0..iter_source.range.get()).map(move |_| {
            offset += iter_source.prime.get();
            if offset >= iter_source.range.get() {
                offset -= iter_source.range.get();
            }

            let index = offset;
            assert!(index < iter_source.range.get());
            index
        })
    }
}
