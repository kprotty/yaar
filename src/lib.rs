#![forbid(unsafe_code)]

use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use once_cell::sync::OnceCell;
use pin_utils::pin_mut;
use std::{
    any::Any,
    cell::{Cell, RefCell},
    future::Future,
    hint::spin_loop,
    mem::{drop, replace},
    num::NonZeroUsize,
    panic::{catch_unwind, resume_unwind, AssertUnwindSafe},
    pin::Pin,
    rc::Rc,
    sync::atomic::{fence, AtomicBool, AtomicU8, AtomicUsize, AtomicIsize, Ordering},
    sync::{Arc, Condvar, Mutex},
    task::{Context, Poll, Wake, Waker},
    thread,
};
use try_lock::TryLock;

pub fn block_on<F: Future>(future: F) -> F::Output {
    pin_mut!(future);

    thread_local!(static TLS_PARKER: Arc<Parker> = Arc::new(Parker::new()));
    let parker = TLS_PARKER.with(|parker| parker.clone());

    assert!(
        !parker.active.load(Ordering::Relaxed),
        "nested calls to block_on() are explicitely not supported",
    );

    let waker = Waker::from(parker.clone());
    let mut ctx = Context::from_waker(&waker);
    parker.active.store(true, Ordering::Relaxed);

    let poll_result = catch_unwind(AssertUnwindSafe(|| loop {
        match future.as_mut().poll(&mut ctx) {
            Poll::Ready(output) => break output,
            Poll::Pending => parker.park(),
        }
    }));

    parker.active.store(false, Ordering::Relaxed);
    match poll_result {
        Ok(output) => output,
        Err(error) => resume_unwind(error),
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
    active: AtomicBool,
    notified: AtomicBool,
    thread: thread::Thread,
}

impl Parker {
    fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            notified: AtomicBool::new(false),
            thread: thread::current(),
        }
    }

    fn park(&self) {
        while !self.notified.swap(false, Ordering::Acquire) {
            thread::park();
        }
    }
}

impl Wake for Parker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref()
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if !self.notified.swap(true, Ordering::Release) {
            self.thread.unpark();
        }
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
}

impl<F> Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn spawn(future: Pin<Box<F>>) -> Arc<Self> {
        let task = Arc::new(Self {
            state: TaskState::default(),
            waker: AtomicWaker::default(),
            data: TryLock::new(TaskData::Empty),
        });

        let waker = Waker::from(task.clone());
        *task.data.try_lock().unwrap() = TaskData::Polling(future, waker);

        Waker::from(task.clone()).wake();
        task
    }
}

impl<F> Wake for Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn wake(self: Arc<Self>) {
        if self.state.transition_to_scheduled() {
            Executor::global().schedule(Scheduled::Wake(self));
        }
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if self.state.transition_to_scheduled() {
            Executor::global().schedule(Scheduled::WakeByRef(self.clone()));
        }
    }
}

trait Runnable: Send + Sync {
    fn run(self: Arc<Self>);
}

impl<F> Runnable for Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn run(self: Arc<Self>) {
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
                return Executor::global().schedule(Scheduled::WakeByRef(self));
            }
        };

        drop(data);
        self.waker.wake();
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

enum Scheduled {
    Wake(Arc<dyn Runnable>),
    WakeByRef(Arc<dyn Runnable>),
}

struct Executor {
    rng_seq: RngSeq,
    state: AtomicUsize,
    semaphore: Semaphore,
    injector: Injector<Arc<dyn Runnable>>,
    stealers: Box<[Stealer<Arc<dyn Runnable>>]>,
}

impl Executor {
    const STATE_BITS: u32 = usize::BITS / 2;
    const STATE_MASK: usize = (1 << Self::STATE_BITS) - 1;

    const IDLE_SHIFT: u32 = Self::STATE_BITS * 0;
    const SEARCH_SHIFT: u32 = Self::STATE_BITS * 1;

    fn global() -> &'static Self {
        static GLOBAL_EXECUTOR: OnceCell<Executor> = OnceCell::new();
        GLOBAL_EXECUTOR.get_or_init(|| {
            let num_threads = num_cpus::get().min(Self::STATE_MASK);
            let num_workers = NonZeroUsize::new(num_threads)
                .or(NonZeroUsize::new(1))
                .unwrap();

            Self {
                rng_seq: RngSeq::new(num_workers),
                state: AtomicUsize::new(0),
                semaphore: Semaphore::default(),
                injector: Injector::new(),
                stealers: (0..num_workers.get())
                    .map(|stealer_index| {
                        let worker = Worker::new_lifo();
                        let stealer = worker.stealer();
                        thread::spawn(move || Self::global().run_worker(worker, stealer_index));
                        stealer
                    })
                    .collect(),
            }
        })
    }

    fn schedule(&self, scheduled: Scheduled) {
        let (runnable, be_fair) = match scheduled {
            Scheduled::Wake(runnable) => (runnable, false),
            Scheduled::WakeByRef(runnable) => (runnable, true),
        };

        if let Some(thread) = Thread::with(|tls| tls.as_ref().map(Rc::clone)) {
            thread.be_fair.set(be_fair || thread.be_fair.get());
            thread.worker.push(runnable);
            return self.notify();
        }

        self.injector.push(runnable);
        fence(Ordering::SeqCst);
        self.notify();
    }

    fn notify(&self) {
        self.state
            .fetch_update(Ordering::Release, Ordering::Relaxed, |state| {
                let searching = (state >> Self::SEARCH_SHIFT) & Self::STATE_MASK;
                assert!(searching <= self.stealers.len());
                if searching > 0 {
                    return None;
                }

                let idle = (state >> Self::IDLE_SHIFT) & Self::STATE_MASK;
                assert!(idle <= self.stealers.len());
                if idle == 0 {
                    return None;
                }

                let mut new_state = state;
                new_state += 1 << Self::SEARCH_SHIFT;
                new_state -= 1 << Self::IDLE_SHIFT;
                return Some(new_state);
            })
            .map(|_| self.semaphore.post())
            .unwrap_or(())
    }

    fn on_worker_search(&self, is_searching: &mut bool) {
        if *is_searching {
            return;
        }

        let state = self.state.load(Ordering::Relaxed);
        let searching = (state >> Self::SEARCH_SHIFT) & Self::STATE_MASK;
        assert!(searching <= self.stealers.len());
        if (2 * searching) >= self.stealers.len() {
            return;
        }

        let state = self
            .state
            .fetch_add(1 << Self::SEARCH_SHIFT, Ordering::Acquire);

        let searching = (state >> Self::SEARCH_SHIFT) & Self::STATE_MASK;
        assert!(searching < self.stealers.len());
        *is_searching = true;
    }

    fn on_worker_discovered(&self, is_searching: &mut bool) {
        if !replace(is_searching, false) {
            return;
        }

        let state = self
            .state
            .fetch_sub(1 << Self::SEARCH_SHIFT, Ordering::Release);

        let searching = (state >> Self::SEARCH_SHIFT) & Self::STATE_MASK;
        assert!(searching <= self.stealers.len());
        assert_ne!(searching, 0);

        if searching == 1 {
            self.notify();
        }
    }

    fn on_worker_idle(&self, is_searching: &mut bool) {
        let was_searching = replace(is_searching, false);

        let update: usize = 1 << Self::IDLE_SHIFT;
        let update = update.wrapping_sub((was_searching as usize) << Self::SEARCH_SHIFT);
        let state = self.state.fetch_add(update, Ordering::AcqRel);

        let idle = (state >> Self::IDLE_SHIFT) & Self::STATE_MASK;
        assert!(idle < self.stealers.len());

        let searching = (state >> Self::SEARCH_SHIFT) & Self::STATE_MASK;
        assert!(searching <= self.stealers.len());
        assert!(searching >= was_searching as usize);

        if was_searching && !self.injector.is_empty() {
            self.notify();
        }
    }

    fn run_worker(&self, worker: Worker<Arc<dyn Runnable>>, stealer_index: usize) {
        let thread = Rc::new(Thread {
            worker,
            be_fair: Cell::new(false),
        });

        let worker = &thread.worker;
        Thread::with(|tls| *tls = Some(thread.clone()));

        let mut rng = Rng::new(stealer_index);
        let mut tick = rng.gen().get();
        let mut searching = false;

        loop {
            let polled = (|| {
                if thread.be_fair.take() || tick % 61 == 0 {
                    if let Steal::Success(runnable) = self.injector.steal() {
                        return Some(runnable);
                    }

                    if let Steal::Success(runnable) = self.stealers[stealer_index].steal() {
                        return Some(runnable);
                    }
                }

                if let Some(runnable) = worker.pop() {
                    return Some(runnable);
                }

                self.on_worker_search(&mut searching);
                if searching {
                    for _attempt in 0..32 {
                        let mut retry = false;
                        for index in self.rng_seq.gen(&mut rng) {
                            let stole = match index {
                                i if i == stealer_index => self.injector.steal_batch_and_pop(&*worker),
                                _ => self.stealers[index].steal_batch_and_pop(&*worker),
                            };

                            match stole {
                                Steal::Success(runnable) => return Some(runnable),
                                Steal::Retry => retry = true,
                                Steal::Empty => {},
                            }
                        }

                        if retry {
                            spin_loop();
                        } else {
                            break;
                        }
                    }
                }

                None
            })();

            if let Some(runnable) = polled {
                self.on_worker_discovered(&mut searching);
                tick = tick.wrapping_add(1);
                runnable.run();
                continue;
            }

            self.on_worker_idle(&mut searching);
            self.semaphore.wait();
            searching = true;
        }
    }
}

struct Thread {
    worker: Worker<Arc<dyn Runnable>>,
    be_fair: Cell<bool>,
}

impl Thread {
    fn with<F>(f: impl FnOnce(&mut Option<Rc<Self>>) -> F) -> F {
        thread_local!(static TLS_WORKER: RefCell<Option<Rc<Thread>>> = RefCell::new(None));
        TLS_WORKER.with(|ref_cell| f(&mut *ref_cell.borrow_mut()))
    }
}

#[derive(Default)]
struct Semaphore {
    count: AtomicIsize,
    value: Mutex<usize>,
    cond: Condvar,
}

impl Semaphore {
    fn wait(&self) {
        let count = self.count.fetch_sub(1, Ordering::Acquire);
        assert_ne!(count, isize::MIN);
        if count > 0 {
            return;
        }

        let mut value = self.value.lock().unwrap();
        value = self.cond.wait_while(value, |v| *v == 0).unwrap();
        *value = value.checked_sub(1).unwrap();
    }

    fn post(&self) {
        let count = self.count.fetch_add(1, Ordering::Release);
        assert_ne!(count, isize::MAX);
        if count >= 0 {
            return;
        }

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
