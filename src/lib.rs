#![forbid(unsafe_code)]

use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::Future,
    mem::{drop, replace},
    num::NonZeroUsize,
    ops::{Deref, DerefMut},
    panic::{catch_unwind, resume_unwind, AssertUnwindSafe},
    pin::Pin,
    rc::Rc,
    sync::atomic::{fence, AtomicBool, AtomicU8, AtomicUsize, Ordering},
    sync::{Arc, Condvar, Mutex},
    task::{Context, Poll, Wake, Waker},
    thread,
    time::Duration,
};

enum RuntimeContext {
    New(Arc<Executor>),
    Current(Thread),
}

impl RuntimeContext {
    fn with_executor<F>(&self, f: impl FnOnce(&Arc<Executor>) -> F) -> F {
        match self {
            RuntimeContext::New(executor) => f(executor),
            RuntimeContext::Current(thread) => f(&thread.executor),
        }
    }
}

pub struct Runtime {
    context: RuntimeContext,
    shutdown: bool,
}

impl Runtime {
    pub fn new(max_threads: usize) -> Self {
        let executor = Arc::new(Executor::new(max_threads));
        Self {
            context: RuntimeContext::New(executor),
            shutdown: true,
        }
    }

    pub fn current() -> Self {
        Self::try_current().expect("Not inside a runtime context")
    }

    pub fn try_current() -> Option<Self> {
        Thread::try_current().map(|thread| Self {
            context: RuntimeContext::Current(thread),
            shutdown: false,
        })
    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        // RIP: no pin_mut! in stdlib
        let future = Box::pin(future);

        self.context.with_executor(|e| e.task_begin());
        let result = WorkerThread::block_on(
            future,
            None,
            match &self.context {
                RuntimeContext::New(executor) => Thread::enter(executor),
                RuntimeContext::Current(thread) => thread.clone(),
            },
        );

        self.context.with_executor(|e| e.task_complete());
        match result {
            Ok(output) => output,
            Err(error) => resume_unwind(error),
        }
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let executor = self.context.with_executor(|e| e.clone());
        executor.task_begin();

        let task = Arc::new(Task {
            state: TaskState::default(),
            waker: AtomicWaker::default(),
            data: Mutex::new(TaskData::Polling(Box::pin(future))),
            executor,
        });

        assert!(task.state.transition_to_scheduled());
        task.executor.schedule(
            task.clone(),
            match &self.context {
                RuntimeContext::New(_) => None,
                RuntimeContext::Current(thread) => Some(thread),
            },
        );

        JoinHandle {
            joinable: Some(task),
        }
    }

    pub fn shutdown_background(self) {
        let mut this = self;
        this.shutdown = false;
        this.context.with_executor(|e| e.shutdown_and_join(None));
    }

    pub fn shutdown_timeout(self, timeout: Duration) {
        let mut this = self;
        this.shutdown = false;
        this.context
            .with_executor(|e| e.shutdown_and_join(Some(Some(timeout))));
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if self.shutdown {
            self.context
                .with_executor(|e| e.shutdown_and_join(Some(None)));
        }
    }
}

pub struct JoinHandle<T> {
    joinable: Option<Arc<dyn Joinable<T>>>,
}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let joinable = self
            .joinable
            .as_ref()
            .expect("JoinHandle polled after completion");

        if let Poll::Ready(output) = joinable.poll_join(ctx) {
            self.joinable = None;
            return Poll::Ready(output);
        }

        Poll::Pending
    }
}

#[derive(Default)]
struct AtomicWaker {
    state: AtomicU8,
    waker: Mutex<Option<Waker>>,
}

impl AtomicWaker {
    const EMPTY: u8 = 0;
    const UPDATING: u8 = 1;
    const READY: u8 = 2;
    const NOTIFIED: u8 = 3;

    fn poll(&self, ctx: &mut Context<'_>) -> Poll<()> {
        let state = self.state.load(Ordering::Acquire);
        match state {
            Self::NOTIFIED => return Poll::Ready(()),
            Self::EMPTY | Self::READY => {}
            _ => unreachable!(),
        }

        if let Err(state) =
            self.state
                .compare_exchange(state, Self::UPDATING, Ordering::Acquire, Ordering::Acquire)
        {
            assert_eq!(state, Self::NOTIFIED);
            return Poll::Ready(());
        }

        {
            let mut waker = self.waker.try_lock().unwrap();
            let will_wake = waker
                .as_ref()
                .map(|w| ctx.waker().will_wake(w))
                .unwrap_or(false);

            if !will_wake {
                *waker = Some(ctx.waker().clone());
            }
        }

        match self.state.compare_exchange(
            Self::UPDATING,
            Self::READY,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Poll::Pending,
            Err(Self::NOTIFIED) => Poll::Ready(()),
            Err(_) => unreachable!(),
        }
    }

    fn wake(&self) {
        match self.state.swap(Self::NOTIFIED, Ordering::AcqRel) {
            Self::READY => {}
            Self::EMPTY | Self::UPDATING => return,
            _ => unreachable!(),
        }

        if let Some(waker) = replace(&mut *self.waker.try_lock().unwrap(), None) {
            waker.wake();
        }
    }
}

#[derive(Default)]
struct TaskState {
    state: AtomicU8,
}

impl TaskState {
    const IDLE: u8 = 0;
    const SCHEDULED: u8 = 1;
    const RUNNING: u8 = 2;
    const NOTIFIED: u8 = 4;

    fn transition_to_scheduled(&self) -> bool {
        self.state
            .fetch_update(Ordering::Release, Ordering::Relaxed, |state| match state {
                Self::IDLE => Some(Self::SCHEDULED),
                Self::RUNNING => Some(Self::NOTIFIED),
                Self::SCHEDULED | Self::NOTIFIED => None,
                _ => unreachable!(),
            })
            .ok()
            .map(|state| state == Self::IDLE)
            .unwrap_or(false)
    }

    fn transition_to_running_from_scheduled(&self) -> bool {
        self.transition_to_running_from(Self::SCHEDULED)
    }

    fn transition_to_running_from_notified(&self) -> bool {
        self.transition_to_running_from(Self::NOTIFIED)
    }

    fn transition_to_running_from(&self, ready_state: u8) -> bool {
        self.state.load(Ordering::Acquire) == ready_state && {
            self.state.store(Self::RUNNING, Ordering::Relaxed);
            true
        }
    }

    fn transition_to_idle(&self) -> bool {
        self.state
            .compare_exchange(
                Self::RUNNING,
                Self::IDLE,
                Ordering::Release,
                Ordering::Relaxed,
            )
            .is_ok()
    }
}

enum TaskData<F: Future> {
    Polling(Pin<Box<F>>),
    Ready(F::Output),
    Error(Box<dyn Any + Send + 'static>),
    Joined,
}

struct Task<F: Future> {
    state: TaskState,
    waker: AtomicWaker,
    data: Mutex<TaskData<F>>,
    executor: Arc<Executor>,
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
            self.clone().schedule()
        }
    }
}

impl<F> Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn schedule(self: Arc<Self>) {
        match Thread::try_current() {
            Some(thread) => thread.executor.schedule(self, Some(&thread)),
            None => self.executor.clone().schedule(self, None),
        }
    }
}

trait Runnable: Send + Sync {
    fn run(self: Arc<Self>, thread: &Thread);
}

impl<F> Runnable for Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn run(self: Arc<Self>, thread: &Thread) {
        assert!(self.state.transition_to_running_from_scheduled());

        let mut data = self.data.try_lock().unwrap();
        let poll_result = match &mut *data {
            TaskData::Polling(future) => {
                let waker = Waker::from(self.clone());
                let mut ctx = Context::from_waker(&waker);
                catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(&mut ctx)))
            }
            _ => unreachable!(),
        };

        *data = match poll_result {
            Err(error) => TaskData::Error(error),
            Ok(Poll::Ready(output)) => TaskData::Ready(output),
            Ok(Poll::Pending) => {
                drop(data);
                if self.state.transition_to_idle() {
                    return;
                }

                assert!(self.state.transition_to_running_from_notified());
                thread.executor.schedule(self, Some(thread));
                return;
            }
        };

        drop(data);
        self.waker.wake();
        thread.executor.task_complete();
    }
}

trait Joinable<T> {
    fn poll_join(&self, ctx: &mut Context<'_>) -> Poll<T>;
}

impl<F: Future> Joinable<F::Output> for Task<F> {
    fn poll_join(&self, ctx: &mut Context<'_>) -> Poll<F::Output> {
        if let Poll::Pending = self.waker.poll(ctx) {
            return Poll::Pending;
        }

        match replace(&mut *self.data.try_lock().unwrap(), TaskData::Joined) {
            TaskData::Error(error) => resume_unwind(error),
            TaskData::Ready(output) => Poll::Ready(output),
            _ => unreachable!(),
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum ParkState {
    Empty,
    Waiting,
    Notified(Option<usize>),
}

impl Into<usize> for ParkState {
    fn into(self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Waiting => 1,
            Self::Notified(index) => {
                let index = index.map(|i| i + 1).unwrap_or(0);
                2 | (index << 2)
            }
        }
    }
}

impl From<usize> for ParkState {
    fn from(value: usize) -> Self {
        match value & 0b11 {
            0 => Self::Empty,
            1 => Self::Waiting,
            2 => Self::Notified((value >> 2).checked_sub(1)),
            _ => unreachable!(),
        }
    }
}

struct Parker {
    state: AtomicUsize,
    parked: AtomicBool,
    thread: thread::Thread,
    task_state: TaskState,
}

impl Parker {
    fn new() -> Self {
        Self {
            state: AtomicUsize::new(ParkState::Empty.into()),
            parked: AtomicBool::new(false),
            thread: thread::current(),
            task_state: TaskState::default(),
        }
    }

    fn reset(&self) {
        let new_state = ParkState::Empty.into();
        self.state.store(new_state, Ordering::Relaxed);
    }

    fn poll(&self) -> Poll<Option<usize>> {
        match ParkState::from(self.state.load(Ordering::Acquire)) {
            ParkState::Notified(worker_index) => Poll::Ready(worker_index),
            _ => Poll::Pending,
        }
    }

    fn park(&self) -> Option<usize> {
        if let Err(state) = self.state.compare_exchange(
            ParkState::Empty.into(),
            ParkState::Waiting.into(),
            Ordering::Acquire,
            Ordering::Acquire,
        ) {
            match ParkState::from(state) {
                ParkState::Notified(worker_index) => return worker_index,
                _ => unreachable!(),
            }
        }

        loop {
            thread::park();
            match self.poll() {
                Poll::Ready(worker_index) => return worker_index,
                Poll::Pending => continue,
            }
        }
    }

    fn unpark(&self, worker_index: Option<usize>) -> bool {
        self.state
            .fetch_update(
                Ordering::Release,
                Ordering::Relaxed,
                |state| match ParkState::from(state) {
                    ParkState::Notified(_) => None,
                    _ => Some(ParkState::Notified(worker_index).into()),
                },
            )
            .map(|state| match ParkState::from(state) {
                ParkState::Waiting => self.thread.unpark(),
                _ => {}
            })
            .is_ok()
    }
}

impl Wake for Parker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if self.task_state.transition_to_scheduled() {
            self.unpark(None);
        }
    }
}

#[derive(Default)]
struct Worker {
    idle_next: AtomicUsize,
    run_queue: Queue<Arc<dyn Runnable>>,
}

struct Executor {
    idle: AtomicUsize,
    tasks: AtomicUsize,
    running: AtomicBool,
    searching: AtomicUsize,
    injector: Queue<Arc<dyn Runnable>>,
    joiner: Condvar,
    rng_seq_seed: RngSeqSeed,
    parked: Mutex<Vec<Arc<Parker>>>,
    workers: Box<[Worker]>,
}

impl Executor {
    fn new(max_threads: usize) -> Self {
        let num_threads = NonZeroUsize::new(max_threads)
            .or(NonZeroUsize::new(1))
            .unwrap();

        let executor = Self {
            idle: AtomicUsize::new(0),
            tasks: AtomicUsize::new(0),
            running: AtomicBool::new(true),
            searching: AtomicUsize::new(0),
            injector: Queue::default(),
            joiner: Condvar::new(),
            rng_seq_seed: RngSeqSeed::new(num_threads),
            parked: Mutex::new(Vec::with_capacity(num_threads.get())),
            workers: (0..num_threads.get()).map(|_| Worker::default()).collect(),
        };

        for worker_index in (0..num_threads.get()).rev() {
            executor.push_idle_worker(worker_index);
        }

        executor
    }

    fn schedule(self: &Arc<Self>, runnable: Arc<dyn Runnable>, thread: Option<&Thread>) {
        if let Some(thread) = thread {
            if let Some(worker_index) = thread.worker_index.get() {
                self.workers[worker_index].run_queue.push(runnable);
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
                .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
        {
            assert!(searching <= self.workers.len());
            return;
        }

        if let Some(worker_index) = self.pop_idle_worker() {
            if self.unpark(Some(worker_index)) {
                return;
            } else {
                self.push_idle_worker(worker_index);
            }
        }

        let searching = self.searching.fetch_sub(1, Ordering::Relaxed);
        assert!(searching <= self.workers.len());
        assert_ne!(searching, 0);
    }

    fn search_begin(&self) -> bool {
        let searching = self.searching.load(Ordering::Relaxed);
        assert!(searching <= self.workers.len());

        if (2 * searching) >= self.workers.len() {
            return false;
        }

        let searching = self.searching.fetch_add(1, Ordering::Acquire);
        assert!(searching < self.workers.len());
        true
    }

    fn search_discovered(self: &Arc<Self>) {
        let searching = self.searching.fetch_sub(1, Ordering::Relaxed);
        assert!(searching <= self.workers.len());
        assert_ne!(searching, 0);

        if searching == 1 {
            self.notify();
        }
    }

    fn search_failed(&self, was_searching: bool, worker_index: usize) -> bool {
        assert!(worker_index < self.workers.len());
        self.push_idle_worker(worker_index);

        was_searching && {
            let searching = self.searching.fetch_sub(1, Ordering::SeqCst);
            assert!(searching <= self.workers.len());
            assert_ne!(searching, 0);
            searching == 1
        }
    }

    fn search_retry(&self) -> Option<usize> {
        self.pop_idle_worker().map(|worker_index| {
            let searching = self.searching.fetch_add(1, Ordering::Acquire);
            assert!(searching < self.workers.len());
            worker_index
        })
    }

    const IDLE_SHIFT: u32 = usize::BITS / 2;
    const IDLE_MASK: usize = (1 << Self::IDLE_SHIFT) - 1;

    fn push_idle_worker(&self, worker_index: usize) {
        let _ = self
            .idle
            .fetch_update(Ordering::Release, Ordering::Relaxed, |idle| {
                assert!(worker_index < self.workers.len());
                assert!(worker_index < Self::IDLE_MASK);

                let top_index = idle & Self::IDLE_MASK;
                self.workers[worker_index]
                    .idle_next
                    .store(top_index, Ordering::Relaxed);

                let new_aba_count = idle >> Self::IDLE_SHIFT;
                let new_top_index = worker_index + 1;
                Some((new_aba_count << Self::IDLE_SHIFT) | new_top_index)
            });
    }

    fn peek_idle_worker(&self) -> Option<usize> {
        let idle = self.idle.load(Ordering::Acquire);
        (idle & Self::IDLE_MASK).checked_sub(1)
    }

    fn pop_idle_worker(&self) -> Option<usize> {
        self.idle
            .fetch_update(Ordering::Acquire, Ordering::Acquire, |idle| {
                let worker_index = (idle & Self::IDLE_MASK).checked_sub(1)?;
                assert!(worker_index < self.workers.len());

                let new_top_index = self.workers[worker_index].idle_next.load(Ordering::Relaxed);
                Some((idle & !Self::IDLE_MASK) | new_top_index)
            })
            .ok()
            .map(|idle| (idle & Self::IDLE_MASK) - 1)
    }

    fn park(&self, parker: &Arc<Parker>) -> Option<usize> {
        if let Poll::Ready(worker_index) = parker.poll() {
            return worker_index;
        }

        {
            let mut parked = self.parked.lock().unwrap();
            if let Poll::Ready(worker_index) = parker.poll() {
                return worker_index;
            }

            if !self.running.load(Ordering::Relaxed) {
                drop(parked);
                return parker.park();
            }

            assert!(!parker.parked.load(Ordering::Relaxed));
            parker.parked.store(true, Ordering::Relaxed);
            parked.push(parker.clone());
        }

        let worker_index = parker.park();

        // If we're still parked, make sure to remove ourselves from the parked Vec
        (|| {
            if !parker.parked.load(Ordering::Relaxed) {
                return;
            }

            let mut parked = self.parked.lock().unwrap();
            if !parker.parked.load(Ordering::Relaxed) {
                return;
            }

            if !self.running.load(Ordering::Relaxed) {
                return;
            }

            for park_index in 0..parked.len() {
                if Arc::ptr_eq(parker, &parked[park_index]) {
                    assert!(parker.parked.load(Ordering::Relaxed));
                    parker.parked.store(false, Ordering::Relaxed);
                    drop(parked.swap_remove(park_index));
                    return;
                }
            }

            unreachable!();
        })();

        worker_index
    }

    fn unpark(self: &Arc<Self>, worker_index: Option<usize>) -> bool {
        {
            let mut parked = self.parked.lock().unwrap();
            if !self.running.load(Ordering::Relaxed) {
                return false;
            }

            while let Some(park_index) = parked.len().checked_sub(1) {
                let parker = parked.swap_remove(park_index);
                assert!(parker.parked.load(Ordering::Relaxed));
                parker.parked.store(false, Ordering::Relaxed);

                drop(parked);
                if parker.unpark(worker_index) {
                    return true;
                }

                parked = self.parked.lock().unwrap();
                if !self.running.load(Ordering::Relaxed) {
                    return false;
                }
            }
        }

        let executor = self.clone();
        thread::Builder::new()
            .spawn(move || {
                // Really with std::future::poll_fn was stable :|
                struct RunningFuture<'a> {
                    executor: &'a Executor,
                }

                impl<'a> Future for RunningFuture<'a> {
                    type Output = ();

                    fn poll(self: Pin<&mut Self>, _ctx: &mut Context<'_>) -> Poll<()> {
                        if self.executor.running.load(Ordering::Acquire) {
                            Poll::Pending
                        } else {
                            Poll::Ready(())
                        }
                    }
                }

                let executor = &executor;
                let mut future = RunningFuture { executor };
                let future = Pin::new(&mut future);

                let thread = Thread::enter(executor);
                WorkerThread::block_on(future, worker_index, thread).unwrap()
            })
            .is_ok()
    }

    fn shutdown_and_join(&self, timeout: Option<Option<Duration>>) {
        let mut parked = self.parked.lock().unwrap();
        assert!(self.running.load(Ordering::Relaxed));
        self.running.store(false, Ordering::Release);

        let pending = replace(&mut *parked, Vec::new());
        drop(parked);
        for parker in pending {
            parker.unpark(None);
        }

        if let Some(timeout) = timeout {
            let guard = self.parked.lock().unwrap();
            let cond = |_: &mut Vec<Arc<Parker>>| self.tasks.load(Ordering::Acquire) > 0;

            if let Some(duration) = timeout {
                let _ = self.joiner.wait_timeout_while(guard, duration, cond);
            } else {
                let _ = self.joiner.wait_while(guard, cond);
            }
        }
    }

    fn task_begin(&self) {
        let tasks = self.tasks.fetch_add(1, Ordering::Relaxed);
        assert_ne!(tasks, usize::MAX);
    }

    fn task_complete(&self) {
        let tasks = self.tasks.fetch_sub(1, Ordering::AcqRel);
        assert_ne!(tasks, 0);

        if tasks == 1 {
            if !self.running.load(Ordering::Acquire) {
                drop(self.parked.lock().unwrap());
                self.joiner.notify_one();
            }
        }
    }
}

struct ThreadContext {
    executor: Arc<Executor>,
    worker_index: Cell<Option<usize>>,
}

impl ThreadContext {
    fn with_tls<F>(f: impl FnOnce(&mut Option<Rc<Self>>) -> F) -> F {
        thread_local!(static TLS: RefCell<Option<Rc<ThreadContext>>> = RefCell::new(None));
        TLS.with(|ref_cell| f(&mut *ref_cell.borrow_mut()))
    }
}

struct Thread {
    context: Rc<ThreadContext>,
}

impl Thread {
    fn enter(executor: &Arc<Executor>) -> Self {
        let context = Rc::new(ThreadContext {
            executor: executor.clone(),
            worker_index: Cell::new(None),
        });

        match ThreadContext::with_tls(|tls| match tls {
            Some(_) => Err(()),
            None => Ok(*tls = Some(context.clone())),
        }) {
            Ok(_) => Self { context },
            Err(_) => unreachable!("Running nested block_on is not supported"),
        }
    }

    fn try_current() -> Option<Self> {
        ThreadContext::with_tls(|tls| {
            tls.as_ref().map(|context| Self {
                context: context.clone(),
            })
        })
    }
}

impl Deref for Thread {
    type Target = ThreadContext;

    fn deref(&self) -> &Self::Target {
        &*self.context
    }
}

impl Clone for Thread {
    fn clone(&self) -> Self {
        Self {
            context: self.context.clone(),
        }
    }
}

impl Drop for Thread {
    fn drop(&mut self) {
        if Rc::strong_count(&self.context) == 2 {
            ThreadContext::with_tls(|tls| *tls = None);
        }
    }
}

struct WorkerThread<F: Future, P: Deref<Target = F> + DerefMut> {
    rng: Rng,
    tick: usize,
    future: Pin<P>,
    thread: Thread,
    searching: bool,
    parker: Arc<Parker>,
}

impl<F: Future, P: Deref<Target = F> + DerefMut> WorkerThread<F, P> {
    fn block_on(
        future: Pin<P>,
        worker_index: Option<usize>,
        thread: Thread,
    ) -> Result<F::Output, Box<dyn Any + Send + 'static>> {
        let mut rng = Rng::new();
        let tick = rng.gen();

        let mut this = Self {
            rng,
            tick,
            future,
            thread,
            searching: false,
            parker: Arc::new(Parker::new()),
        };

        if let Some(worker_index) = worker_index.or_else(|| this.thread.executor.search_retry()) {
            this.transition_to_notified(worker_index);
        }

        assert!(this.parker.task_state.transition_to_scheduled());
        let result = this.block_on_worker();

        if let Some(worker_index) = this.thread.worker_index.get() {
            this.transition_to_idle(worker_index);
        }

        result
    }

    fn block_on_worker(&mut self) -> Result<F::Output, Box<dyn Any + Send + 'static>> {
        loop {
            let retry_poll_future = match self.poll_future() {
                Poll::Ready(Some(result)) => return result,
                Poll::Ready(None) => true,
                Poll::Pending => false,
            };

            if let Some(runnable) = self.poll_runnable() {
                if replace(&mut self.searching, false) {
                    self.thread.executor.search_discovered();
                }

                self.tick = self.tick.wrapping_add(1);
                runnable.run(&self.thread);
                continue;
            }

            if retry_poll_future {
                continue;
            }

            let worker_index = self.thread.executor.park(&self.parker);
            self.parker.reset();

            if let Some(worker_index) = worker_index  {
                self.transition_to_notified(worker_index);
            }
        }
    }

    fn poll_future(&mut self) -> Poll<Option<Result<F::Output, Box<dyn Any + Send + 'static>>>> {
        if !self
            .parker
            .task_state
            .transition_to_running_from_scheduled()
        {
            return Poll::Pending;
        }

        let poll_result = {
            let waker = Waker::from(self.parker.clone());
            let mut ctx = Context::from_waker(&waker);
            catch_unwind(AssertUnwindSafe(|| self.future.as_mut().poll(&mut ctx)))
        };

        match poll_result {
            Err(error) => Poll::Ready(Some(Err(error))),
            Ok(Poll::Ready(output)) => Poll::Ready(Some(Ok(output))),
            Ok(Poll::Pending) => {
                if self.parker.task_state.transition_to_idle() {
                    return Poll::Pending;
                }

                assert!(self.parker.task_state.transition_to_running_from_notified());
                Poll::Ready(None)
            }
        }
    }

    fn poll_runnable(&mut self) -> Option<Arc<dyn Runnable>> {
        loop {
            let worker_index = self.thread.worker_index.get()?;
            let executor = &self.thread.executor;
            let run_queue = &executor.workers[worker_index].run_queue;

            if self.tick % 61 == 0 {
                if let Steal::Success(runnable) = executor.injector.steal() {
                    return Some(runnable);
                }

                if let Steal::Success(runnable) = run_queue.steal() {
                    return Some(runnable);
                }
            }

            if let Some(runnable) = run_queue.pop() {
                return Some(runnable);
            }

            self.searching = self.searching || executor.search_begin();
            if self.searching {
                for _attempt in 0..32 {
                    let mut is_empty = match executor.injector.steal_into(run_queue) {
                        Steal::Success(runnable) => return Some(runnable),
                        Steal::Retry => false,
                        Steal::Empty => true,
                    };

                    for steal_index in self.rng.gen_seq(executor.rng_seq_seed) {
                        assert!(steal_index < executor.workers.len());
                        if steal_index == worker_index {
                            continue;
                        }

                        let steal_queue = &executor.workers[steal_index].run_queue;
                        match steal_queue.steal_into(run_queue) {
                            Steal::Success(runnable) => return Some(runnable),
                            Steal::Retry => is_empty = false,
                            Steal::Empty => {}
                        }
                    }

                    if is_empty {
                        break;
                    }
                }
            }

            drop(run_queue);
            drop(executor);
            self.transition_to_idle(worker_index);
        }
    }

    fn transition_to_notified(&mut self, worker_index: usize) {
        assert_eq!(self.thread.worker_index.get(), None);
        self.thread.worker_index.set(Some(worker_index));

        assert!(!self.searching);
        self.searching = true;
    }

    fn transition_to_idle(&mut self, worker_index: usize) {
        assert_eq!(self.thread.worker_index.get(), Some(worker_index));
        self.thread.worker_index.set(None);

        let was_searching = replace(&mut self.searching, false);
        let last_searching = self
            .thread
            .executor
            .search_failed(was_searching, worker_index);

        if last_searching && !self.thread.executor.injector.is_empty() {
            if let Some(worker_index) = self.thread.executor.search_retry() {
                self.transition_to_notified(worker_index);
            }
        }
    }
}

#[derive(Copy, Clone)]
struct RngSeqSeed {
    range: NonZeroUsize,
    prime: NonZeroUsize,
}

impl RngSeqSeed {
    fn new(range: NonZeroUsize) -> Self {
        Self {
            range,
            prime: ((range.get() / 2)..range.get())
                .rev()
                .filter(|&n| Self::gcd(n, range.get()) == 1)
                .next()
                .and_then(|n| NonZeroUsize::new(n))
                .unwrap(),
        }
    }

    fn gcd(mut a: usize, mut b: usize) -> usize {
        while a != b {
            if a > b {
                a -= b;
            } else {
                b -= a;
            }
        }
        a
    }
}

struct Rng {
    xorshift: NonZeroUsize,
}

impl Rng {
    fn new() -> Self {
        static SEED: AtomicUsize = AtomicUsize::new(0);
        let seed = SEED.fetch_add(1, Ordering::Relaxed);

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

    fn gen(&mut self) -> usize {
        let shifts = match usize::BITS {
            64 => (13, 7, 17),
            32 => (13, 17, 5),
            _ => unreachable!(),
        };

        let mut xs = self.xorshift.get();
        xs ^= xs << shifts.0;
        xs ^= xs >> shifts.1;
        xs ^= xs << shifts.2;

        self.xorshift = NonZeroUsize::new(xs).unwrap();
        xs
    }

    fn gen_seq(&mut self, seed: RngSeqSeed) -> impl Iterator<Item = usize> {
        let mut index = self.gen() % seed.range.get();
        (0..seed.range.get()).map(move |_| {
            let mut new_index = index + seed.prime.get();
            if new_index >= seed.range.get() {
                new_index -= seed.range.get();
            }

            assert!(new_index < seed.range.get());
            replace(&mut index, new_index)
        })
    }
}

enum Steal<T> {
    Success(T),
    Empty,
    Retry,
}

struct Queue<T> {
    pending: AtomicBool,
    deque: Mutex<VecDeque<T>>,
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Self {
            pending: AtomicBool::new(false),
            deque: Mutex::new(VecDeque::new()),
        }
    }
}

impl<T> Queue<T> {
    fn is_empty(&self) -> bool {
        !self.pending.load(Ordering::Acquire)
    }

    fn push(&self, item: T) {
        let mut deque = self.deque.lock().unwrap();
        deque.push_back(item);
        self.pending.store(true, Ordering::Relaxed);
    }

    fn pop(&self) -> Option<T> {
        if self.is_empty() {
            return None;
        }

        let mut deque = self.deque.lock().unwrap();
        deque.pop_back().map(|item| {
            self.pending.store(deque.len() > 0, Ordering::Relaxed);
            item
        })
    }

    fn steal(&self) -> Steal<T> {
        if self.is_empty() {
            return Steal::Empty;
        }

        let mut deque = match self.deque.try_lock() {
            Ok(deque) => deque,
            Err(_) => return Steal::Retry,
        };

        let item = match deque.pop_front() {
            Some(item) => item,
            None => return Steal::Empty,
        };

        self.pending.store(deque.len() > 0, Ordering::Relaxed);
        Steal::Success(item)
    }

    fn steal_into(&self, dst_queue: &Self) -> Steal<T> {
        if self.is_empty() {
            return Steal::Empty;
        }

        let mut deque = match self.deque.try_lock() {
            Ok(deque) => deque,
            Err(_) => return Steal::Retry,
        };

        let item = match deque.pop_front() {
            Some(item) => item,
            None => return Steal::Empty,
        };

        let migrate = (deque.len() / 2).min(8);
        if migrate > 0 {
            let mut dst_deque = dst_queue.deque.lock().unwrap();
            dst_deque.extend(deque.drain(0..migrate));
            dst_queue.pending.store(true, Ordering::Relaxed);
        }

        self.pending.store(deque.len() > 0, Ordering::Relaxed);
        Steal::Success(item)
    }
}
