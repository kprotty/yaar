use core::{
    convert::{AsMut, AsRef},
    future::Future,
    hint::unreachable_unchecked,
    marker::PhantomPinned,
    mem::{align_of, MaybeUninit},
    pin::Pin,
    ptr::{null, NonNull},
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

    /// Polls a given future using the task's resume function for waking.
    pub unsafe fn poll<F: Future>(&mut self, future: &mut F) -> Poll<F::Output> {
        const WAKE: unsafe fn(*const ()) = |ptr| unsafe {
            let executor = (&*super::EXECUTOR_CELL.0.as_ptr()).as_ref().unwrap();
            executor.schedule(ptr as *mut Task);
        };

        const VTABLE: RawWakerVTable = RawWakerVTable::new(
            |ptr| RawWaker::new(ptr, &VTABLE),
            WAKE,
            WAKE,
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
