#![forbid(unsafe_code)]

use std::{
    any::Any,
    cell::{Cell, RefCell},
    future::Future,
    hint::spin_loop,
    mem::{drop, replace},
    num::NonZeroUsize,
    ops::Deref,
    panic::{catch_unwind, resume_unwind, AssertUnwindSafe},
    pin::Pin,
    rc::Rc,
    sync::atomic::{fence, AtomicBool, AtomicU8, AtomicUsize, Ordering},
    sync::{Arc, Condvar, Mutex},
    task::{Context, Poll, Wake, Waker},
    thread,
};
use try_lock::TryLock;
use crossbeam_deque as Deque;

pub struct Executor {
    worker_threads: usize,
}

impl Executor {
    pub fn with_threads(worker_threads: usize) -> Self {
        Self { worker_threads }
    }

    pub fn block_on<F: Future>(self, future: F) -> F::Output {
        let mut future = Box::pin(future);
        let scheduler = Arc::new(Scheduler::new(self.worker_threads));
        let thread = Thread::enter(scheduler, None);

        let parker = Arc::new(Parker::new());
        let waker = Waker::from(parker.clone());
        let mut ctx = Context::from_waker(&waker);

        thread.scheduler.on_task_begin();
        let poll_result = catch_unwind(AssertUnwindSafe(|| loop {
            match future.as_mut().poll(&mut ctx) {
                Poll::Ready(output) => break output,
                Poll::Pending => parker.park(),
            }
        }));

        thread.scheduler.on_task_complete();
        match poll_result {
            Ok(output) => output,
            Err(error) => resume_unwind(error),
        }
    }
}

pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let future = Box::pin(future);
    let task = Task::spawn(future);
    JoinHandle(Some(task))
}

pub struct JoinHandle<T>(Option<Arc<dyn Joinable<T>>>);

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let joinable = self.0.take().expect("JoinHandle polled after completion");

        if let Poll::Ready(output) = joinable.poll_join(ctx) {
            return Poll::Ready(output);
        }

        self.0 = Some(joinable);
        Poll::Pending
    }
}

struct Parker {
    notified: AtomicBool,
    thread: thread::Thread,
}

impl Parker {
    fn new() -> Self {
        Self {
            notified: AtomicBool::new(false),
            thread: thread::current(),
        }
    }

    fn park(&self) {
        loop {
            match self.notified.load(Ordering::Acquire) {
                true => return self.notified.store(false, Ordering::Relaxed),
                false => thread::park(),
            }
        }
    }
}

impl Wake for Parker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref()
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.notified.store(true, Ordering::Release);
        self.thread.unpark();
    }
}

#[derive(Default)]
struct AtomicWaker {
    state: AtomicU8,
    waker: TryLock<Option<Waker>>,
}

impl AtomicWaker {
    const EMPTY: u8 = 0;
    const UPDATING: u8 = 1;
    const READY: u8 = 2;
    const NOTIFIED: u8 = 3;

    fn poll(&self, ctx: &mut Context<'_>) -> Poll<()> {
        let state = self.state.load(Ordering::Acquire);
        match state {
            Self::EMPTY | Self::READY => {}
            Self::NOTIFIED => return Poll::Ready(()),
            Self::UPDATING => unreachable!("AtomicWaker polled by multiple threads"),
            _ => unreachable!("invalid AtomicWaker state"),
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
            Err(_) => unreachable!("invalid AtomicWaker state"),
        }
    }

    fn wake(&self) {
        match self.state.swap(Self::NOTIFIED, Ordering::AcqRel) {
            Self::READY => {}
            Self::EMPTY | Self::UPDATING => return,
            Self::NOTIFIED => unreachable!("AtomicWaker awoken multiple times"),
            _ => unreachable!("invalid AtomicWaker state"),
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
    const NOTIFIED: u8 = 3;

    fn transition_to_scheduled(&self) -> bool {
        self.state
            .fetch_update(Ordering::Release, Ordering::Relaxed, |state| match state {
                Self::SCHEDULED | Self::NOTIFIED => None,
                Self::RUNNING => Some(Self::NOTIFIED),
                Self::IDLE => Some(Self::SCHEDULED),
                _ => unreachable!("invalid TaskState"),
            })
            .map(|state| state == Self::IDLE)
            .unwrap_or(false)
    }

    fn transition_to_running(&self) {
        assert_eq!(self.state.load(Ordering::Acquire), Self::SCHEDULED);
        self.state.store(Self::RUNNING, Ordering::Relaxed);
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

    fn transition_to_scheduled_from_notified(&self) {
        assert_eq!(self.state.load(Ordering::Acquire), Self::NOTIFIED);
        self.state.store(Self::SCHEDULED, Ordering::Relaxed);
    }
}

enum TaskData<F: Future> {
    Empty,
    Polling(Pin<Box<F>>, Waker),
    Ready(F::Output),
    Panic(Box<dyn Any + Send + 'static>),
    Joined,
}

struct Task<F: Future> {
    state: TaskState,
    waker: AtomicWaker,
    data: TryLock<TaskData<F>>,
    scheduler: Arc<Scheduler>,
}

impl<F> Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn spawn(future: Pin<Box<F>>) -> Arc<Self> {
        let thread = Thread::current().expect("spawn() called outside the Executor context");

        let task = Arc::new(Self {
            state: TaskState::default(),
            waker: AtomicWaker::default(),
            data: TryLock::new(TaskData::Empty),
            scheduler: thread.scheduler.clone(),
        });

        let waker = Waker::from(task.clone());
        *task.data.try_lock().unwrap() = TaskData::Polling(future, waker);

        thread.scheduler.on_task_begin();
        Waker::from(task.clone()).wake();
        task
    }

    fn schedule(self: Arc<Self>) {
        match Thread::current() {
            Some(thread) => thread.scheduler.schedule(self, Some(&thread), false),
            None => self.scheduler.schedule(self.clone(), None, false),
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
            Arc::clone(self).schedule();
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
        let mut data = self.data.try_lock().unwrap();
        self.state.transition_to_running();

        let poll_result = match &mut *data {
            TaskData::Polling(future, waker) => {
                let future = future.as_mut();
                let mut ctx = Context::from_waker(waker);
                catch_unwind(AssertUnwindSafe(|| future.poll(&mut ctx)))
            }
            _ => unreachable!("invalid TaskData when polling"),
        };

        *data = match poll_result {
            Err(error) => TaskData::Panic(error),
            Ok(Poll::Ready(output)) => TaskData::Ready(output),
            Ok(Poll::Pending) => {
                drop(data);
                if self.state.transition_to_idle() {
                    return;
                }

                self.state.transition_to_scheduled_from_notified();
                return thread.scheduler.schedule(self, Some(thread), true);
            }
        };

        drop(data);
        self.waker.wake();
        thread.scheduler.on_task_complete();
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
            TaskData::Ready(output) => Poll::Ready(output),
            TaskData::Panic(error) => resume_unwind(error),
            _ => unreachable!("invalid TaskData when joining"),
        }
    }
}

struct Scheduler {
    rng_seq: RngSeq,
    state: AtomicUsize,
    tasks: AtomicUsize,
    injector: QueueInjector,
    semaphore: Semaphore,
    workers: Box<[Worker]>,
}

impl Scheduler {
    const STATE_BITS: u32 = usize::BITS / 3;
    const STATE_MASK: usize = (1 << Self::STATE_BITS) - 1;

    const IDLE_SHIFT: u32 = Self::STATE_BITS * 0;
    const SPAWN_SHIFT: u32 = Self::STATE_BITS * 1;
    const SEARCH_SHIFT: u32 = Self::STATE_BITS * 2;

    const PADDING_BITS: u32 = usize::BITS % 3;
    const SHUTDOWN_SHIFT: u32 = usize::BITS - Self::PADDING_BITS;

    fn new(mut worker_threads: usize) -> Self {
        worker_threads = worker_threads.min(Self::STATE_MASK);
        let num_workers = NonZeroUsize::new(worker_threads)
            .or(NonZeroUsize::new(1))
            .unwrap();

        Self {
            rng_seq: RngSeq::new(num_workers),
            state: AtomicUsize::new(0),
            tasks: AtomicUsize::new(0),
            injector: QueueInjector::new(),
            semaphore: Semaphore::default(),
            workers: (0..num_workers.get()).map(|_| Worker::new()).collect(),
        }
    }

    fn schedule(self: &Arc<Self>, runnable: Arc<dyn Runnable>, thread: Option<&Thread>, be_fair: bool) {
        if let Some(thread) = thread {
            thread.be_fair.set(be_fair);
            if let Some(queue_worker) = thread.queue_worker.as_ref() {
                queue_worker.push(runnable);
                return self.notify();
            }
        }

        self.injector.push(runnable);
        fence(Ordering::SeqCst);
        self.notify();
    }

    fn notify(self: &Arc<Self>) {
        self.state
            .fetch_update(Ordering::Release, Ordering::Relaxed, |state| {
                let new_state = state + (1 << Self::SEARCH_SHIFT);
                if state & (1 << Self::SHUTDOWN_SHIFT) != 0 {
                    return None;
                }

                let searching = (state >> Self::SEARCH_SHIFT) & Self::STATE_MASK;
                assert!(searching <= self.workers.len());
                if searching > 0 {
                    return None;
                }

                let idle = (state >> Self::IDLE_SHIFT) & Self::STATE_MASK;
                assert!(idle <= self.workers.len());
                if idle > 0 {
                    return Some(new_state - (1 << Self::IDLE_SHIFT));
                }

                let spawned = (state >> Self::SPAWN_SHIFT) & Self::STATE_MASK;
                assert!(spawned <= self.workers.len());
                if spawned < self.workers.len() {
                    return Some(new_state + (1 << Self::SPAWN_SHIFT));
                }

                None
            })
            .map(|state| {
                let idle = (state >> Self::IDLE_SHIFT) & Self::STATE_MASK;
                assert!(idle <= self.workers.len());
                if idle > 0 {
                    return self.semaphore.post();
                }

                let spawned = (state >> Self::SPAWN_SHIFT) & Self::STATE_MASK;
                assert!(spawned < self.workers.len());

                let worker_index = spawned;
                let scheduler = Arc::clone(self);
                thread::spawn(move || Worker::run(scheduler, worker_index));
            })
            .unwrap_or(())
    }

    fn on_worker_search(&self) -> bool {
        // let state = self.state.load(Ordering::Relaxed);
        // if state & (1 << Self::SHUTDOWN_SHIFT) != 0 {
        //     return false;
        // }

        // let searching = (state >> Self::SEARCH_SHIFT) & Self::STATE_MASK;
        // assert!(searching <= self.workers.len());
        // if 2 * state >= self.workers.len() {
        //     return false;
        // }

        let state = self
            .state
            .fetch_add(1 << Self::SEARCH_SHIFT, Ordering::Acquire);

        let searching = (state >> Self::SEARCH_SHIFT) & Self::STATE_MASK;
        assert!(searching < self.workers.len());
        true
    }

    fn on_worker_discovered(self: &Arc<Self>, was_searching: bool) {
        if !was_searching {
            return;
        }

        let state = self
            .state
            .fetch_sub(1 << Self::SEARCH_SHIFT, Ordering::Release);

        let searching = (state >> Self::SEARCH_SHIFT) & Self::STATE_MASK;
        assert!(searching <= self.workers.len());
        assert_ne!(searching, 0);

        if searching == 1 {
            self.notify();
        }
    }

    fn on_worker_idle(self: &Arc<Self>, was_searching: bool) -> bool {
        let update: usize = 1 << Self::IDLE_SHIFT;
        let update = update.wrapping_sub((was_searching as usize) << Self::SEARCH_SHIFT);
        let state = self.state.fetch_add(update, Ordering::SeqCst);

        let idle = (state >> Self::IDLE_SHIFT) & Self::STATE_MASK;
        assert!(idle < self.workers.len());

        let searching = (state >> Self::SEARCH_SHIFT) & Self::STATE_MASK;
        assert!(searching <= self.workers.len());
        assert!(searching >= was_searching as usize);

        if !self.injector.is_empty() {
            self.notify();
        }

        state & (1 << Self::SHUTDOWN_SHIFT) == 0
    }

    fn on_task_begin(&self) {
        let tasks = self.tasks.fetch_add(1, Ordering::Relaxed);
        assert_ne!(tasks, usize::MAX);
    }

    fn on_task_complete(&self) {
        let tasks = self.tasks.fetch_sub(1, Ordering::Release);
        assert_ne!(tasks, 0);

        if tasks == 1 {
            fence(Ordering::Acquire);
            self.shutdown();
        }
    }

    fn shutdown(&self) {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |mut state| {
                let idle = (state >> Self::IDLE_SHIFT) & Self::STATE_MASK;
                assert!(idle <= self.workers.len());
                state -= idle << Self::IDLE_SHIFT;

                let searching = (state >> Self::SEARCH_SHIFT) & Self::STATE_MASK;
                assert!(searching <= self.workers.len());
                state += idle << Self::SEARCH_SHIFT;

                assert_eq!(state & (1 << Self::SHUTDOWN_SHIFT), 0);
                Some(state | (1 << Self::SHUTDOWN_SHIFT))
            })
            .map(|state| {
                let idle = (state >> Self::IDLE_SHIFT) & Self::STATE_MASK;
                for _idle_worker in 0..idle {
                    self.semaphore.post();
                }
            })
            .unwrap_or(())
    }
}

type QueueSteal = Deque::Steal<Arc<dyn Runnable>>;
type QueueWorker = Deque::Worker<Arc<dyn Runnable>>;
type QueueStealer = Deque::Stealer<Arc<dyn Runnable>>;
type QueueInjector = Deque::Injector<Arc<dyn Runnable>>;

struct Worker {
    queue_stealer: QueueStealer,
    queue_worker: TryLock<Option<QueueWorker>>,
}

impl Worker {
    fn new() -> Self {
        let queue_worker = QueueWorker::new_lifo();
        let queue_stealer = queue_worker.stealer();

        Self {
            queue_worker: TryLock::new(Some(queue_worker)),
            queue_stealer,
        }
    }

    fn run(scheduler: Arc<Scheduler>, worker_index: usize) {
        let mut rng = Rng::new(worker_index);
        let mut tick = rng.gen().get();
        let mut searching = true;

        let thread = Thread::enter(scheduler, Some(worker_index));
        let queue_worker = thread.queue_worker.as_ref().unwrap();
        let scheduler = &thread.scheduler;

        loop {
            let polled = (|| {
                let be_fair = thread.be_fair.take() || tick % 61 == 0;
                if be_fair {
                    if let QueueSteal::Success(runnable) = scheduler.injector.steal() {
                        return Some(runnable);
                    }

                    let queue_stealer = &scheduler.workers[worker_index].queue_stealer;
                    if let QueueSteal::Success(runnable) = queue_stealer.steal() {
                        return Some(runnable);
                    }
                }

                if let Some(runnable) = queue_worker.pop() {
                    return Some(runnable);
                }

                searching = searching || scheduler.on_worker_search();
                if searching {
                    for _attempt in 0..32 {
                        let mut was_contended =
                            match scheduler.injector.steal_batch_and_pop(&*queue_worker) {
                                QueueSteal::Success(runnable) => return Some(runnable),
                                QueueSteal::Retry => true,
                                QueueSteal::Empty => false,
                            };

                        for steal_index in scheduler.rng_seq.gen(&mut rng) {
                            if steal_index == worker_index {
                                continue;
                            }

                            let queue_stealer = &scheduler.workers[steal_index].queue_stealer;
                            match queue_stealer.steal_batch_and_pop(&*queue_worker) {
                                QueueSteal::Success(runnable) => return Some(runnable),
                                QueueSteal::Retry => was_contended = true,
                                QueueSteal::Empty => {}
                            }
                        }

                        if was_contended {
                            spin_loop()
                        } else {
                            break;
                        }
                    }
                }

                None
            })();

            let was_searching = replace(&mut searching, false);
            if let Some(runnable) = polled {
                scheduler.on_worker_discovered(was_searching);
                tick = tick.wrapping_add(1);
                runnable.run(&thread);
                continue;
            }

            if scheduler.on_worker_idle(was_searching) {
                scheduler.semaphore.wait();
                searching = true;
                continue;
            }

            return;
        }
    }
}

struct ThreadContext {
    be_fair: Cell<bool>,
    scheduler: Arc<Scheduler>,
    queue_worker: Option<QueueWorker>,
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
    fn enter(scheduler: Arc<Scheduler>, worker_index: Option<usize>) -> Self {
        let queue_worker = worker_index.map(|worker_index| {
            let worker = &scheduler.workers[worker_index];
            let mut queue_worker = worker.queue_worker.try_lock().unwrap();
            replace(&mut *queue_worker, None).unwrap()
        });
        
        ThreadContext::with_tls(|tls| {
            let context = Rc::new(ThreadContext {
                be_fair: Cell::new(false),
                queue_worker,
                scheduler,
            });

            let old_tls = replace(tls, Some(context.clone()));
            assert!(old_tls.is_none(), "Nested thread blocking is not supported");
            Self { context }
        })
    }

    fn current() -> Option<Self> {
        ThreadContext::with_tls(|tls| tls.as_ref().map(Rc::clone)).map(|context| Self { context })
    }
}

impl Deref for Thread {
    type Target = ThreadContext;

    fn deref(&self) -> &Self::Target {
        &*self.context
    }
}

impl Drop for Thread {
    fn drop(&mut self) {
        if Rc::strong_count(&self.context) == 2 {
            let old_tls = ThreadContext::with_tls(|tls| replace(tls, None));
            let old_tls = old_tls.expect("Thread dropped without ThreadContext");
            assert!(Rc::ptr_eq(&self.context, &old_tls));
        }
    }
}

#[derive(Default)]
struct Semaphore {
    value: Mutex<usize>,
    cond: Condvar,
}

impl Semaphore {
    fn wait(&self) {
        let mut value = self.value.lock().unwrap();
        value = self.cond.wait_while(value, |v| *v == 0).unwrap();
        *value = value.checked_sub(1).unwrap();
    }

    fn post(&self) {
        let mut value = self.value.lock().unwrap();
        *value = value.checked_add(1).unwrap();
        self.cond.notify_one();
    }
}

struct Rng {
    xorshift: NonZeroUsize,
}

impl Rng {
    fn new(seed: usize) -> Self {
        Self {
            xorshift: NonZeroUsize::new(seed)
                .or(NonZeroUsize::new(0xdeadbeef))
                .unwrap(),
        }
    }

    fn gen(&mut self) -> NonZeroUsize {
        let shifts = match usize::BITS {
            64 => (13, 17, 5),
            32 => (13, 7, 17),
            _ => unreachable!("unsupported platform"),
        };

        let mut xs = self.xorshift.get();
        xs ^= xs << shifts.0;
        xs ^= xs >> shifts.1;
        xs ^= xs << shifts.2;

        self.xorshift = NonZeroUsize::new(xs).unwrap();
        self.xorshift
    }
}

#[derive(Copy, Clone)]
struct RngSeq {
    range: NonZeroUsize,
    prime: NonZeroUsize,
}

impl RngSeq {
    fn new(range: NonZeroUsize) -> Self {
        Self {
            range,
            prime: (range.get() / 2..range.get())
                .filter(|&n| Self::gcd(n, range.get()) == 1)
                .next()
                .and_then(NonZeroUsize::new)
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

    fn gen(self, rng: &mut Rng) -> impl Iterator<Item = usize> {
        let Self { range, prime } = self;
        let mut index = rng.gen().get() % range.get();

        (0..range.get()).map(move |_| {
            let mut next_index = index + prime.get();
            next_index -= range.get() * ((next_index >= range.get()) as usize);
            assert!(next_index < range.get());
            replace(&mut index, next_index)
        })
    }
}
