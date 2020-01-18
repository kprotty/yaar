use yaar_lock::sync::{RawMutex, ThreadParker};
use core::{
    convert::{AsMut, AsRef},
    future::Future,
    hint::unreachable_unchecked,
    marker::PhantomPinned,
    mem::{align_of, MaybeUninit},
    pin::Pin,
    ptr::{null, NonNull},
    sync::atomic::{spin_loop_hint, Ordering},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

/// The importance of a task which
/// may be used to influence scheduling.
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum Priority {
    Low = 0,
    Normal = 1,
    High = 2,
}

impl Priority {
    const MASK: usize = 3;
}

/// Minimal state used to represent a generic
/// unit of execution used by the executors in the runtime.
pub struct Task {
    next: usize,
    resume: unsafe fn(*mut Task),
}

impl Task {
    /// Create a new task with the given priority and resume function.
    /// The resume function is called when the thread is woken up by the
    /// scheduler internally to start executing its corresponding unit (not limited to futures).
    pub fn new(priority: Priority, resume: unsafe fn(*mut Task)) -> Self {
        assert!(align_of::<Self>() > Priority::MASK);
        let next = priority as usize;
        Self { next, resume }
    }

    /// Get the link provided by the task as it can function as a linked list node.
    pub fn next(&self) -> Option<NonNull<Self>> {
        NonNull::new((self.next & !Priority::MASK) as *mut Self)
    }

    /// Get the next task link having the task act as a linked list node.
    pub fn set_next(&mut self, next: Option<NonNull<Self>>) {
        self.next = (self.next & Priority::MASK)
            | match next {
                Some(ptr) => ptr.as_ptr() as usize,
                None => null::<Self>() as usize,
            };
    }

    /// Modify the runtime priority of the task.
    /// Should only be called before the task is resumed as
    /// the scheduler could also have ownership internally.
    pub fn set_priority(&mut self, priority: Priority) {
        self.next = (self.next & !Priority::MASK) | (priority as usize);
    }

    /// Get the assigned priority of the task.
    pub fn priority(&self) -> Priority {
        match self.next & Priority::MASK {
            0 => Priority::Low,
            1 => Priority::Normal,
            2 => Priority::High,
            _ => unsafe { unreachable_unchecked() },
        }
    }

    /// Call the resume function provided by `new`
    /// using the given task as a parameter.
    pub unsafe fn resume(&mut self) {
        (self.resume)(self)
    }

    /// Schedule the task on the executor to be eventually ran.
    ///
    /// # Panic
    /// 
    /// This function panics if there is no current running executor.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the same task must not be scheduled more
    /// than once before it is resumed. This may result in resume being called
    /// on multiple threads as is therefor undefiend behavior. 
    pub unsafe fn schedule(&mut self) {
        let executor = (&*super::EXECUTOR_CELL.0.as_ptr()).as_ref().unwrap();
        (executor._schedule)(executor.ptr, self)
    }

    /// Polls a given future using the task, scheduling the task on wake in order 
    /// to run its resume function.
    ///
    /// # Safety
    ///
    /// Similar to [`schedule`], the caller must ensure that the task is not
    /// polled more than once before being resumed.
    pub unsafe fn poll<F: Future>(&mut self, future: &mut F) -> Poll<F::Output> {
        const VTABLE: RawWakerVTable = RawWakerVTable::new(
            |ptr| RawWaker::new(ptr, &VTABLE),
            |ptr| unsafe { (&mut *(ptr as *mut Task)).schedule() },
            |ptr| unsafe { (&mut *(ptr as *mut Task)).schedule() },
            |_| {},
        );

        let task = self as *mut Self as *const ();
        let raw_waker = RawWaker::new(task, &VTABLE);
        let waker = Waker::from_raw(raw_waker);
        let mut ctx = Context::from_waker(&waker);

        Pin::new_unchecked(future).poll(&mut ctx)
    }
}

/// The combination a future with a [`Task`] in order 
/// to run the future using the runtime executor.
pub struct FutureTask<F: Future> {
    task: Task,
    future: F,
    pinned: PhantomPinned,
}

unsafe impl<F: Future + Send> Send for FutureTask<F> {}
unsafe impl<F: Future + Sync> Sync for FutureTask<F> {}

impl<F: Future> AsRef<Task> for FutureTask<F> {
    fn as_ref(&self) -> &Task {
        &self.task
    }
}

impl<F: Future> AsMut<Task> for FutureTask<F> {
    fn as_mut(&mut self) -> &mut Task {
        &mut self.task
    }
}

impl<F: Future> Future for FutureTask<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        unsafe { self.map_unchecked_mut(|this| &mut this.future).poll(ctx) }
    }
}

impl<F: Future> FutureTask<F> {
    /// Create a new Task + Future structure.
    ///
    /// The priority of the task is specified up front but can be
    /// modified through a direct reference to the task from `as_mut`.
    pub fn new(priority: Priority, future: F) -> Self {
        Self {
            future,
            pinned: PhantomPinned,
            task: Task::new(priority, |task| unsafe {
                // Self(task - offsetof(Self, "task")).resume()
                let stub = MaybeUninit::<Self>::zeroed();
                let base_ptr = stub.as_ptr() as usize;
                let task_ptr = &(&*stub.as_ptr()).task as *const _ as usize;
                let self_ptr = ((task as usize) - (task_ptr - base_ptr)) as *mut Self;
                let _ = (&mut *self_ptr).resume();
            }),
        }
    }

    /// Poll the future using the inner task
    pub fn resume(&mut self) -> Poll<F::Output> {
        unsafe { self.task.poll(&mut self.future) }
    }
}

/// The combination a future with a [`Task`] in order to run the future
/// using the runtime executor while making the output of the
/// future retrivable by caching it.
pub struct CachedFutureTask<F: Future> {
    task: Task,
    output: Option<F::Output>,
    future: F,
    pinned: PhantomPinned,
}

unsafe impl<F: Future + Send> Send for CachedFutureTask<F> {}
unsafe impl<F: Future + Sync> Sync for CachedFutureTask<F> {}

impl<F: Future> AsRef<Task> for CachedFutureTask<F> {
    fn as_ref(&self) -> &Task {
        &self.task
    }
}

impl<F: Future> AsMut<Task> for CachedFutureTask<F> {
    fn as_mut(&mut self) -> &mut Task {
        &mut self.task
    }
}

impl<F: Future> CachedFutureTask<F> {
    /// Create a new Task + Future structure which caches the future's output.
    ///
    /// The priority of the task is specified up front but can be
    /// modified through a direct reference to the task from `as_mut`.
    pub fn new(priority: Priority, future: F) -> Self {
        Self {
            future,
            output: None,
            pinned: PhantomPinned,
            task: Task::new(priority, |task| unsafe {
                // Self(task - offsetof(Self, "task")).resume()
                let stub = MaybeUninit::<Self>::zeroed();
                let base_ptr = stub.as_ptr() as usize;
                let task_ptr = &(&*stub.as_ptr()).task as *const _ as usize;
                let self_ptr = ((task as usize) - (task_ptr - base_ptr)) as *mut Self;
                let _ = (&mut *self_ptr).resume();
            }),
        }
    }

    /// Poll the future using the inner task,
    /// returning a reference to the output if the future is completed.
    ///
    /// Unlike [`FutureTask`], this may be called multiple times as the
    /// output of the inner future is cached.
    pub fn resume(&mut self) -> Option<&mut F::Output> {
        if self.output.is_some() {
            return self.output.as_mut();
        }

        match unsafe { self.task.poll(&mut self.future) } {
            Poll::Pending => None,
            Poll::Ready(output) => {
                self.output = Some(output);
                self.output.as_mut()
            }
        }
    }

    /// Consume the `CachedFutureTask` and return its inner output.
    pub fn into_inner(self) -> Option<F::Output> {
        self.output
    }
}

/// Returns a future which can be used to yield the current
/// future to the executor in order to let another task run.
///
/// Takes in a `Priority` to potentially influence
/// the re-scheduling of the current future task.
pub fn yield_now(priority: Priority) -> impl Future {
    struct YieldTask {
        did_yield: bool,
    };

    impl Future for YieldTask {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
            if self.did_yield {
                return Poll::Ready(());
            }

            self.did_yield = true;
            ctx.waker().wake_by_ref();
            Poll::Pending
        }
    }

    FutureTask::new(priority, YieldTask { did_yield: false })
}

#[derive(Default)]
pub(super) struct List {
    head: Option<NonNull<Task>>,
    tail: Option<NonNull<Task>>,
}

impl List {
    pub fn pop<'a>(&mut self) -> Option<&'a mut Task> {
        self.head.map(|task| {
            let task = unsafe { &mut *task.as_ptr() };
            self.head = task.next();
            if self.head.is_none() {
                self.tail = None;
            }
            task
        })
    }

    pub fn push(&mut self, priority_list: &PriorityList) {
        self.push_front(&priority_list.front);
        self.push_back(&priority_list.back);
    }

    pub fn push_front(&mut self, list: &Self) {
        if let Some(tail) = list.tail {
            unsafe { (&mut *tail.as_ptr()).set_next(self.head) };
        }
        if self.tail.is_none() {
            self.tail = list.tail;
        }
        self.head = list.head;
    }

    pub fn push_back(&mut self, list: &Self) {
        if let Some(tail) = self.tail {
            unsafe { (&mut *tail.as_ptr()).set_next(list.head) };
        }
        if self.head.is_none() {
            self.head = list.head;
        }
        self.tail = list.tail;
    }
}

#[derive(Default)]
pub(super) struct PriorityList {
    front: List,
    back: List,
    pub size: usize,
}

impl PriorityList {
    pub fn pop<'a>(&mut self) -> Option<&'a mut Task> {
        self.front.pop().or_else(|| self.back.pop()).map(|task| {
            self.size -= 1;
            task
        })
    }

    pub fn push(&mut self, task: &mut Task) {
        self.size += 1;
        let list = List {
            head: NonNull::new(task),
            tail: NonNull::new(task),
        };
        match task.priority() {
            Priority::Low | Priority::Normal => self.back.push_back(&list),
            Priority::High => self.front.push_back(&list),
        }
    }
}

#[derive(Default)]
pub(super) struct Injector {
    list: List,
    size: usize,
}

impl Injector {
    pub fn push(&mut self, list: &PriorityList) {
        self.list.push(list);
        self.size += list.size;
    }

    pub fn pop<'a>(&mut self, queue: &mut LocalQueue, queue_count: usize, max: usize) -> Option<&'a mut Task> {
        if self.size == 0 {
            return None;
        }

        let mut tail = *queue.tail().as_mut();
        let mut head = queue.head().load(Ordering::Acquire);
        let mut size = (self.size / queue_count)
            .min(tail.wrapping_sub(head))
            .max(max);

        let task = self.list.pop();
        self.size -= size;
        size -= 1;
        if size != 0 {
            debug_assert_eq!(tail.wrapping_sub(head), 0);
        }

        for i in 0..size {
            if let Some(task) = self.list.pop() {
                match task.priority() {
                    Priority::Low,
                    Priority::Normal => {
                        queue.tasks[tail % QUEUE_SIZE] = MaybeUninit::new(task);
                        tail = tail.wrapping_add(1);
                    },
                    Priority::High => {
                        queue.tasks[head.wrapping_sub(1) % QUEUE_SIZE] = MaybeUninit::new(task);
                        head = head.wrapping_sub(1);
                    },
                }
            } else {
                break;
            }
        }

        let pos = unsafe { &*(queue.head() as *const _ as *const AtomicPos) };
        pos.store(unsafe { transmute([head, tail]) }, Ordering::Release);
        Some(unsafe { &* task })
    }
}

use self::queue_index::*;

#[cfg(target_pointer_width = "32")]
mod queue_index {
    pub type Index = u16;
    pub type AtomicPos = core::sync::atomic::AtomicU32;
    pub type AtomicIndex = core::sync::atomic::AtomicU16;
}

#[cfg(target_pointer_width = "64")]
mod queue_index {
    pub type Index = u32;
    pub type AtomicPos = core::sync::atomic::AtomicU64;
    pub type AtomicIndex = core::sync::atomic::AtomicU32;
}

const QUEUE_SIZE: usize = 256;
pub(super) struct LocalQueue {
    pos: [Index; 2],
    tasks: [MaybeUninit<*mut Task>; QUEUE_SIZE],
}

impl Default for LocalQueue {
    fn default() -> Self {
        Self {
            pos: [Index::new(0), Index::new(0)],
            tasks: [MaybeUninit::uninit(); QUEUE_SIZE],
        }
    }
}

impl LocalQueue {
    #[inline(always)]
    pub fn head(&mut self) -> &mut AtomicIndex {
        &self.pos[0];
    }

    #[inline(always)]
    pub fn tail(&mut self) -> &mut AtomicIndex {
        &self.pos[1];
    }

    pub fn size(&self) -> usize {
        let tail = self.tail().load(Ordering::Acquire);
        let head = self.head().load(Ordering::Acquire);
        tail.wrapping_sub(head)
    }

    pub fn push<P: ThreadParker>(&mut self, task: &mut Task, injector: &RawMutex<Injector, P>) {
        unsafe {
            match task.priority() {
                Priority::Low,
                Priority::Normal => self.push_back(task),
                Priority::High => self.push_front(task),
            }
        }
    }

    unsafe fn push_back<P: ThreadParker>(&mut self, task: &mut Task, injector: &RawMutex<Injector, P>) {
        loop {
            let tail = *self.tail().get_mut();
            let head = self.head().load(Ordering::Acquire);

            if tail.wrapping_sub(head) < QUEUE_SIZE {
                self.tasks[tail % QUEUE_SIZE] = MaybeUninit::new(task);
                self.tail().store(tail.wrapping_add(1), Ordering::Release);
                return;
            }

            if self.push_overflow(task, injector) {
                return;
            } else {
                spin_loop_hint();
            }
        }
    }

    unsafe fn push_front<P: ThreadParker>(&mut self, task: &mut Task, injector: &RawMutex<Injector, P>) {
        loop {
            let tail = *self.tail().get_mut();
            let head = self.head().load(Ordering::Acquire);

            if tail.wrapping_sub(head) < QUEUE_SIZE {
                self.tasks[head.wrapping_sub(1) % QUEUE_SIZE] = MaybeUninit::new(task);
                if let Ok(_) = self.head().compare_exchange_weak(
                    head,
                    head.wrapping_sub(1),
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    return;
                }
                spin_loop_hint();
                continue;
            }

            if self.push_overflow(task, injector, head) {
                return;
            } else {
                spin_loop_hint();
            }
        }
    }

    unsafe fn push_overflow<P: ThreadParker>(&mut self, task: &mut Task, injector: &RawMutex<Injector, P>, head: Index) -> bool {
        let grab = QUEUE_SIZE / 2;
        if self.head().compare_exchange_weak(
            head,
            head.wrapping_add(grab),
            Ordering::Acquire,
            Ordering::Relaxed,
        ).is_err() {
            return false;
        }

        let mut list = PriorityList::default();
        list.push(task);
        for i in 0..grab {
            list.push(self.tasks[head.wrapping_add(i) % QUEUE_SIZE].assume_init());
        }
        injector.lock().push(&list);
    }

    pub fn pop<'a>(&mut self) -> Option<&'a mut Task> {
        loop {
            let tail = *self.tail().get_mut();
            let head = self.head().load(Ordering::Acquire);
            if tail.wrapping_sub(head) == 0 {
                return None;
            }

            let task = self.tasks[head % QUEUE_SIZE].assume_init();
            match self.head().compare_exchange_weak(
                head,
                head.wrapping_add(1),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(task),
                Err(_) => spin_loop_hint(),
            }
        }
    }

    pub fn pop_back<'a>(&mut self) -> Option<&'a mut Task> {
        loop {
            let tail = *self.tail().get_mut();
            let head = self.head().load(Ordering::Acquire);
            if tail.wrapping_sub(head) == 0 {
                return None;
            }

            let task = self.tasks[tail.wrapping_sub(1) % QUEUE_SIZE].assume_init();
            let pos = unsafe { &*(self.head() as *const _ as *const AtomicPos) };
            match pos.compare_exchange_weak(
                unsafe { transmute([head, tail]) },
                unsafe { transmute([head, tail.wrapping_sub(1)]) },
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(unsafe { &*task }),
                Err(_) => spin_loop_hint(),
            }
        }
    }

    pub fn steal<'a>(&mut self, other: &mut LocalQueue) -> Option<&'a mut Task> {
        let t = *self.tail().get_mut();
        let h = self.head().load(Ordering::Acquire);
        debug_assert!(t.wrapping_sub(h) == 0);

        loop {
            let head = other.head().load(Ordering::Acquire);
            let tail = other.tail().load(Ordering::Acquire);
            let size = tail.wrapping_sub(head);
            let size = size - (size / 2);
            if size == 0 {
                return None;
            }

            for i in 0..size {
                let task = other.tasks[head.wrapping_add(i) % QUEUE_SIZE];
                self.tasks[t.wrapping_add(i) % QUEUE_SIZE] = task;
            }

            if other.head().compare_exchange_weak(
                head,
                head.wrapping_add(size),
                Ordering::Release,
                Ordering::Relaxed,
            ).is_err() {
                spin_loop_hint();
                continue;
            }

            let task = self.tasks[t % QUEUE_SIZE].assume_init();
            self.tail().store(t.wrapping_add(size - 1), Ordering::Release);
            return Some(task);
        }
    }
}
