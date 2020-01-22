use super::get_executor_ref;
use core::{
    convert::{AsMut, AsRef},
    future::Future,
    hint::unreachable_unchecked,
    marker::PhantomPinned,
    mem::{self, align_of, MaybeUninit},
    num::{NonZeroU64, NonZeroUsize},
    pin::Pin,
    ptr::NonNull,
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};
use yaar_lock::sync::{RawMutex, ThreadParker};

macro_rules! field_parent_ptr {
    ($type:ty, $field:ident, $ptr:expr) => {{
        // TODO: A non-UB way of offsetof() on stable?
        let stub = MaybeUninit::<$type>::zeroed();
        let base = stub.as_ptr() as usize;
        let field = &(*stub.as_ptr()).$field as *const _ as usize;
        &mut *((($ptr) as usize - (field - base)) as *mut $type)
    }};
}

/// Importance level of a task used to influence its scheduling delay.
///
/// The scheduler will make an effort to resume `High` priority tasks
/// before `Normal` priority tasks, and so on.
#[derive(Copy, Clone, PartialEq)]
pub enum TaskPriority {
    Low = 0,
    Normal = 1,
    High = 2,
}

impl TaskPriority {
    pub const MASK: usize = 3;
}

/// A unit of execution to the runtime scheduler.
///
/// Code that wishes to run concurrently should express itself
/// by units of tasks. This is separate from [`Future`]s and it
/// allows finer-grain and dynamic approaches to code execution.
///
/// For tasks integrated with [`Future`]s, see [`FutureTask`].
pub struct Task {
    next: usize,
    resume: unsafe fn(*mut Self),
}

unsafe impl Send for Task {}

impl Task {
    /// Create a new task using the provided priority
    /// and a function to be called when resumed.
    ///
    /// The resume function uses a pointer to the current
    /// task in order to continue execution. It can be
    /// invoked manually through [`resume`].
    ///
    /// [`resume`]: struct.Task.html#method.resume
    pub fn new(priority: TaskPriority, resume: unsafe fn(*mut Self)) -> Self {
        debug_assert!(align_of::<Self>() > TaskPriority::MASK);
        Self {
            next: priority as usize,
            resume,
        }
    }

    /// Tasks also function as intrusive linked list nodes.
    ///
    /// Set the next field of the task to another task pointer.
    pub(super) fn set_next(&mut self, task: Option<NonNull<Self>>) {
        let task = task.map(|ptr| ptr.as_ptr() as usize).unwrap_or(0);
        self.next = (self.next & TaskPriority::MASK) | task;
    }

    /// Tasks also function as intrusive linked list nodes.
    ///
    /// Set the next field of the task to another task pointer.
    pub(super) fn next(&self) -> Option<NonNull<Self>> {
        NonNull::new((self.next & !TaskPriority::MASK) as *mut Self)
    }

    /// Modify the [`TaskPriority`] of the current task.
    ///
    /// It is undefined behavior to update the priority before the
    /// task has been resumed after being [`schedule`]'d.
    ///
    /// [`schedule`]: struct.Task.html#method.schedule
    pub fn set_priority(&mut self, priority: TaskPriority) {
        self.next = (self.next & !TaskPriority::MASK) | (priority as usize);
    }

    /// Get the [`TaskPriority`] of the task.
    pub fn priority(&self) -> TaskPriority {
        match self.next & TaskPriority::MASK {
            0 => TaskPriority::Low,
            1 => TaskPriority::Normal,
            2 => TaskPriority::High,
            _ => unsafe {
                debug_assert!(false, "invalid TaskPriority");
                unreachable_unchecked()
            },
        }
    }

    /// Invoke the resume function of the task manually.
    /// This may be useful for implementing custom scheduling.
    ///
    /// # Safety
    ///
    /// The caller should ensure that the task isn't already scheduled
    /// onto the executor and that the resume function used to create
    /// the task is inheritly safe.
    #[inline]
    pub unsafe fn resume(&mut self) {
        (self.resume)(self)
    }

    /// Schedule the given task onto the executor in order to eventually
    /// call it's resume function for execution. If there is no current
    /// running executor, it does nothing.
    ///
    /// # Safety
    ///
    /// As this interacts with executor internals directly, the caller
    /// should uphold that the task's resume function is both safe to
    /// call and that the task has not been scheduled more than once
    /// before it's resume was called. If this were the case, the executor
    /// could call the resume function in paralell causing undefined behaviour.
    pub unsafe fn schedule(&mut self) {
        if let Some(e) = get_executor_ref() {
            (e.schedule)(e.ptr, self)
        }
    }

    /// Poll a given future using the task as a [`Waker`] implementation which
    /// calls [`schedule`] on the task once woken through `wake()` or `wake_by_ref()`.
    ///
    /// # Safety
    ///
    /// The caller should ensure the same rules as [`schedule`] with regards
    /// to the safety of poll(). The task should also ideally have a method
    /// of continuing the future as well but is not required.
    ///
    /// [`schedule`]: struct.Task.html#method.schedule
    pub unsafe fn poll<F: Future>(&mut self, future: Pin<&mut F>) -> Poll<F::Output> {
        const VTABLE: RawWakerVTable = RawWakerVTable::new(
            |ptr| RawWaker::new(ptr, &VTABLE),
            |ptr| unsafe { (&mut *(ptr as *mut Task)).schedule() },
            |ptr| unsafe { (&mut *(ptr as *mut Task)).schedule() },
            |_| {},
        );

        let raw_waker = RawWaker::new(self as *const Self as *const (), &VTABLE);
        let waker = Waker::from_raw(raw_waker);
        let mut ctx = Context::from_waker(&waker);
        future.poll(&mut ctx)
    }
}

/// A [`Future`] associated with a [`Task`].
///
/// This is the primary method used to express concurrency
/// for futures using the runtime's task system.
pub struct FutureTask<F> {
    pinned: PhantomPinned,
    future: F,
    task: Task,
}

impl<F> AsRef<Task> for FutureTask<F> {
    fn as_ref(&self) -> &Task {
        &self.task
    }
}

impl<F> AsMut<Task> for FutureTask<F> {
    fn as_mut(&mut self) -> &mut Task {
        &mut self.task
    }
}

impl<F: Future> Future for FutureTask<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        // this is safe as the Future should be !Unpin from the PhantomPinned
        unsafe { self.map_unchecked_mut(|this| &mut this.future).poll(ctx) }
    }
}

impl<F: Future> FutureTask<F> {
    /// Create a new task using the priority and associated using the given future.
    pub fn new(priority: TaskPriority, future: F) -> Self {
        Self {
            pinned: PhantomPinned,
            future,
            task: Task::new(priority, |task_ptr| unsafe {
                // To resume: using a ptr to task, find Self via offsetof() and poll() again.
                let this = field_parent_ptr!(Self, task, task_ptr);
                let _ = this.task.poll(Pin::new_unchecked(&mut this.future));
            }),
        }
    }

    /// Associate a future with an already existing task.
    ///
    /// # Safety
    ///
    /// It is expected that when polling, the task's resume
    /// function should have a method of re-polling the future.
    /// If not, the future could possibly dead-lock and never resolve.
    pub unsafe fn from_task(task: Task, future: F) -> Self {
        Self {
            pinned: PhantomPinned,
            future,
            task,
        }
    }
}

/// A [`Future`] and [`Task`] association similar to [`FutureTask`]
/// but caches the output of the future's poll internally.
pub struct CachedFutureTask<F: Future> {
    pinned: PhantomPinned,
    output: Option<F::Output>,
    future: F,
    task: Task,
}

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

impl<F: Future> Future for CachedFutureTask<F> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _ctx: &mut Context<'_>) -> Poll<Self::Output> {
        // return ready if the output has already been cached/consumed.
        if self.output.is_some() {
            return Poll::Ready(());
        }

        // see `FutureTask` above
        unsafe {
            let this = self.get_unchecked_mut();
            let pinned = Pin::new_unchecked(&mut this.future);
            match this.task.poll(pinned) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(output) => {
                    this.output = Some(output);
                    Poll::Ready(())
                }
            }
        }
    }
}

impl<F: Future> CachedFutureTask<F> {
    /// Create a new task + future association with an empty cached output.
    pub fn new(priority: TaskPriority, future: F) -> Self {
        Self {
            pinned: PhantomPinned,
            output: None,
            future,
            task: Task::new(priority, |task_ptr| unsafe {
                let this = field_parent_ptr!(Self, task, task_ptr);
                let _ = (&mut *task_ptr).poll(Pin::new_unchecked(this));
            }),
        }
    }

    /// Check the state of the output. Also used as a method of
    /// checking for the future's completion.
    pub fn peek(&self) -> Option<&F::Output> {
        self.output.as_ref()
    }

    /// Consume the cached output, returning it if set by the completed future.
    pub fn into_inner(self) -> Option<F::Output> {
        self.output
    }
}

pub fn yield_now(priority: TaskPriority) -> impl Future<Output = ()> {
    struct YieldFuture {
        pinned: PhantomPinned,
        waker: Option<Waker>,
        yielded: bool,
        task: Task,
    }

    impl Future for YieldFuture {
        type Output = ();

        fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
            if self.yielded {
                return Poll::Ready(());
            }

            // it is safe to get_unchecked_mut()/schedule() since
            // the future is !Unpin and wont move.
            unsafe {
                let this = self.get_unchecked_mut();
                this.waker = Some(ctx.waker().clone());
                this.yielded = true;
                this.task.schedule();
                Poll::Pending
            }
        }
    }

    YieldFuture {
        pinned: PhantomPinned,
        waker: None,
        yielded: false,
        task: Task::new(priority, |task_ptr| unsafe {
            let this = field_parent_ptr!(YieldFuture, task, task_ptr);
            mem::replace(&mut this.waker, None).unwrap().wake();
        }),
    }
}

/// Run the given function without blocking the executor.
///
/// `max_block_ms` can be used as a hint in milliseconds for how long the function
/// is allowed to block before the runtime possibly spawns a new thread to handle
/// other tasks. Providing a value of `None` is akin to having a timeout of 0ms.
///
/// A future with the result is returned as it needs to wait to start running under
/// the runtime's executor again if the `max_block_ms` timeout ran out.
pub fn run_blocking<T>(
    max_block_ms: Option<NonZeroU64>,
    f: impl FnOnce() -> T,
) -> impl Future<Output = T> {
    // TODO
    unreachable!();

    struct Stub<T>(T);
    impl<T> Future for Stub<T> {
        type Output = T;
        fn poll(self: Pin<&mut Self>, _ctx: &mut Context<'_>) -> Poll<Self::Output> {
            Poll::Pending
        }
    }
    Stub(f())
}
