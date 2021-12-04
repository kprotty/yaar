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
        let result = Worker::block_on(
            future,
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
        executor.task_complete();

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

struct Parker {
    state: AtomicU8,
    parked: AtomicBool,
    thread: thread::Thread,
    task_state: TaskState,
}

impl Parker {
    const EMPTY: u8 = 0;
    const WAITING: u8 = 1;
    const NOTIFIED: u8 = 2;

    fn new() -> Self {
        Self {
            state: AtomicU8::new(Self::EMPTY),
            parked: AtomicBool::new(false),
            thread: thread::current(),
            task_state: TaskState::default(),
        }
    }

    fn unparked(&self) -> bool {
        match self.state.load(Ordering::Acquire) {
            Self::EMPTY | Self::WAITING => false,
            Self::NOTIFIED => true,
            _ => unreachable!(),
        }
    }

    fn park(&self) {
        match self.state.compare_exchange(
            Self::EMPTY,
            Self::WAITING,
            Ordering::Acquire,
            Ordering::Acquire,
        ) {
            Err(Self::NOTIFIED) => {}
            Err(_) => unreachable!(),
            Ok(_) => loop {
                thread::park();
                match self.state.load(Ordering::Acquire) {
                    Self::WAITING => continue,
                    Self::NOTIFIED => break,
                    _ => unreachable!(),
                }
            },
        }

        self.state.store(Self::EMPTY, Ordering::Relaxed);
    }

    fn unpark(&self) -> bool {
        match self.state.swap(Self::NOTIFIED, Ordering::Release) {
            Self::EMPTY => {}
            Self::WAITING => self.thread.unpark(),
            Self::NOTIFIED => return false,
            _ => unreachable!(),
        }
        true
    }
}

impl Wake for Parker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if self.task_state.transition_to_scheduled() {
            self.unpark();
        }
    }
}

struct Executor {
    idle: Queue<usize>,
    injector: Queue<Arc<dyn Runnable>>,
    tasks: AtomicUsize,
    running: AtomicBool,
    searching: AtomicUsize,
    joiner: Condvar,
    rng_seq_seed: RngSeqSeed,
    parked: Mutex<Vec<Arc<Parker>>>,
    run_queues: Box<[Queue<Arc<dyn Runnable>>]>,
}

impl Executor {
    fn new(max_threads: usize) -> Self {
        let num_threads = NonZeroUsize::new(max_threads)
            .or(NonZeroUsize::new(1))
            .unwrap();

        let executor = Self {
            idle: Queue::default(),
            injector: Queue::default(),
            tasks: AtomicUsize::new(0),
            running: AtomicBool::new(true),
            searching: AtomicUsize::new(0),
            joiner: Condvar::new(),
            rng_seq_seed: RngSeqSeed::new(num_threads),
            parked: Mutex::new(Vec::with_capacity(num_threads.get())),
            run_queues: (0..num_threads.get()).map(|_| Queue::default()).collect(),
        };

        for queue_index in (0..num_threads.get()).rev() {
            executor.idle.push(queue_index);
        }

        executor
    }

    fn schedule(self: &Arc<Self>, runnable: Arc<dyn Runnable>, thread: Option<&Thread>) {
        if let Some(thread) = thread {
            if let Some(queue_index) = thread.queue_index.get() {
                self.run_queues[queue_index].push(runnable);
                self.notify();
                return;
            }
        }

        self.injector.push(runnable);
        fence(Ordering::SeqCst);
        self.notify();
    }

    fn notify(self: &Arc<Self>) {
        if self.idle.is_empty() {
            return;
        }

        let searching = self.searching.load(Ordering::Relaxed);
        assert!(searching <= self.run_queues.len());
        if searching > 0 {
            return;
        }

        if let Err(searching) =
            self.searching
                .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
        {
            assert!(searching <= self.run_queues.len());
            return;
        }

        if self.unpark() {
            return;
        }

        let searching = self.searching.fetch_sub(1, Ordering::Relaxed);
        assert!(searching <= self.run_queues.len());
        assert_ne!(searching, 0);
    }

    fn search_begin(&self) -> bool {
        let searching = self.searching.load(Ordering::Relaxed);
        assert!(searching <= self.run_queues.len());

        (2 * searching) < self.run_queues.len() && {
            let searching = self.searching.fetch_add(1, Ordering::Acquire);
            assert!(searching < self.run_queues.len());
            true
        }
    }

    fn search_complete(&self) -> bool {
        let searching = self.searching.fetch_sub(1, Ordering::Relaxed);
        assert!(searching <= self.run_queues.len());
        assert_ne!(searching, 0);
        searching == 1
    }

    fn is_empty(&self) -> bool {
        self.run_queues
            .iter()
            .map(|worker| !worker.is_empty())
            .find(|&is_not_empty| is_not_empty)
            .unwrap_or(false)
    }

    fn park(&self, parker: &Arc<Parker>) {
        if parker.unparked() {
            return;
        }

        {
            let mut parked = self.parked.lock().unwrap();
            if parker.unparked() {
                return;
            }

            if !self.running.load(Ordering::Relaxed) {
                drop(parked);
                parker.park();
                return;
            }

            assert!(!parker.parked.load(Ordering::Relaxed));
            parker.parked.store(true, Ordering::Relaxed);
            parked.push(parker.clone());
        }

        parker.park();
        if !parker.parked.load(Ordering::Relaxed) {
            return;
        }

        let mut parked = self.parked.lock().unwrap();
        if !parker.parked.load(Ordering::Relaxed) {
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

        unreachable!()
    }

    fn unpark(self: &Arc<Self>) -> bool {
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
                if parker.unpark() {
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
                struct ThreadPoolFuture<'a> {
                    executor: &'a Executor,
                }

                impl<'a> Future for ThreadPoolFuture<'a> {
                    type Output = ();

                    fn poll(self: Pin<&mut Self>, _ctx: &mut Context<'_>) -> Poll<()> {
                        if self.executor.running.load(Ordering::Acquire) {
                            Poll::Pending
                        } else {
                            Poll::Ready(())
                        }
                    }
                }

                let mut future = ThreadPoolFuture {
                    executor: &executor,
                };
                let future = Pin::new(&mut future);

                let thread = Thread::enter(&executor);
                Worker::block_on(future, thread).unwrap()
            })
            .is_ok()
    }

    fn shutdown_and_join(&self, timeout: Option<Option<Duration>>) {
        let mut parked = self.parked.lock().unwrap();
        assert!(!self.running.load(Ordering::Relaxed));
        self.running.store(false, Ordering::Release);

        let pending = replace(&mut *parked, Vec::new());
        drop(parked);
        for parker in pending {
            parker.unpark();
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
        let tasks = self.tasks.fetch_sub(1, Ordering::Release);
        assert_ne!(tasks, 0);

        if tasks == 1 {
            drop(self.parked.lock().unwrap());
            self.joiner.notify_one();
        }
    }
}

struct ThreadContext {
    executor: Arc<Executor>,
    queue_index: Cell<Option<usize>>,
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
            queue_index: Cell::new(None),
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

struct Worker<F: Future, P: Deref<Target = F> + DerefMut> {
    rng: Rng,
    tick: usize,
    future: Pin<P>,
    thread: Thread,
    searching: bool,
    parker: Arc<Parker>,
}

impl<F: Future, P: Deref<Target = F> + DerefMut> Worker<F, P> {
    fn block_on(
        future: Pin<P>,
        thread: Thread,
    ) -> Result<F::Output, Box<dyn Any + Send + 'static>> {
        let mut rng = Rng::new();
        let tick = rng.gen();

        let mut worker = Self {
            rng,
            tick,
            future,
            thread,
            searching: false,
            parker: Arc::new(Parker::new()),
        };

        worker.try_transition_to_notified();
        let result = worker.block_on_worker();

        worker.try_transition_to_idle();
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
                    if self.thread.executor.search_complete() {
                        self.thread.executor.notify();
                    }
                }

                self.tick = self.tick.wrapping_add(1);
                runnable.run(&self.thread);
                continue;
            }

            if retry_poll_future {
                continue;
            }

            self.thread.executor.park(&self.parker);
            if let Some(queue_index) = self.thread.executor.idle.pop() {
                self.transition_to_notified(queue_index);
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
            let queue_index = self.thread.queue_index.get()?;
            let executor = &self.thread.executor;
            let run_queue = &executor.run_queues[queue_index];

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
                        assert!(steal_index < executor.run_queues.len());
                        if steal_index == queue_index {
                            continue;
                        }

                        match executor.run_queues[steal_index].steal_into(run_queue) {
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
            self.transition_to_idle(queue_index);
        }
    }

    fn try_transition_to_notified(&mut self) -> bool {
        self.thread
            .executor
            .idle
            .pop()
            .map(|queue_index| {
                self.transition_to_notified(queue_index);
            })
            .is_some()
    }

    fn transition_to_notified(&mut self, queue_index: usize) {
        assert_eq!(self.thread.queue_index.get(), None);
        self.thread.queue_index.set(Some(queue_index));

        assert!(!self.searching);
        self.searching = self.thread.executor.search_begin();
    }

    fn try_transition_to_idle(&mut self) -> bool {
        self.thread
            .queue_index
            .get()
            .map(|queue_index| {
                self.transition_to_idle(queue_index);
            })
            .is_some()
    }

    fn transition_to_idle(&mut self, queue_index: usize) {
        assert_eq!(self.thread.queue_index.get(), Some(queue_index));
        self.thread.queue_index.set(None);

        self.thread.executor.idle.push(queue_index);
        let was_searching = replace(&mut self.searching, false);
        let search_retry = was_searching && self.thread.executor.search_complete();

        if search_retry && !self.thread.executor.is_empty() {
            self.try_transition_to_notified();
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

        let migrate = (deque.len() / 2).min(16);
        if migrate > 0 {
            let mut dst_deque = dst_queue.deque.lock().unwrap();
            dst_deque.extend(deque.drain(0..migrate));
            dst_queue.pending.store(true, Ordering::Relaxed);
        }

        self.pending.store(deque.len() > 0, Ordering::Relaxed);
        Steal::Success(item)
    }
}
