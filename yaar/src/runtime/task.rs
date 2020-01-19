
use core::{
    convert::{AsRef, AsMut},
    ptr::NonNull,
    pin::Pin,
    mem::{align_of, MaybeUninit},
    marker::PhantomPinned,
    future::Future,
    task::{Poll, Context, Waker, RawWaker, RawWakerVTable},
};

#[derive(Copy, Clone, PartialEq)]
pub enum TaskPriority {
    Normal = 0,
    High = 1,
}

pub struct Task {
    next: usize,
    resume: unsafe fn(*mut Self),
}

unsafe impl Send for Task {}

impl Task {
    // NOTE: When chaning the `TaskPriority`, also change the mask to match
    const MASK: usize = !1;

    pub fn new(priority: TaskPriority, resume: unsafe fn(*mut Self)) -> Self {
        assert!(align_of::<Self>() > !Self::MASK);
        Self {
            next: priority as usize,
            resume,
        }
    }

    pub(super) fn set_next(&mut self, task: Option<NonNull<Self>>) {
        let task = task.map(|ptr| ptr.as_ptr() as usize).unwrap_or(0);
        self.next = (self.next & !Self::MASK) | task;
    }

    pub(super) fn next(&self) -> Option<NonNull<Self>> {
        NonNull::new((self.next & Self::MASK) as *mut Self)
    }

    pub fn set_priority(&mut self, priority: TaskPriority) {
        self.next = (self.next & Self::MASK) | (priority as usize);
    }

    pub fn priority(&self) -> TaskPriority {
        match self.next & !Self::MASK {
            0 => TaskPriority::Normal,
            1 => TaskPriority::High,
            _ => unreachable!(),
        }
    }

    #[inline]
    pub unsafe fn resume(&mut self) {
        (self.resume)(self)
    }

    /// Schedule the given task onto the executor in order to eventually
    /// call it's resume function for execution.
    ///
    /// # Safety
    ///
    /// The caller should ensure that the following criteria are met wben
    /// invoking `schedule` in order to avoid undefined behavior:
    /// 
    /// * this is called in the scope of a running executor.
    /// * the resume function on the task is safe to call
    /// * the task hasn't been scheduled more than once before being resumed.
    pub unsafe fn schedule(&mut self) {
        super::with_executor_ref(|executor_ref| {
            (executor_ref.schedule)(executor_ref.ptr, self)
        })
    }

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

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<F::Output> {
        // this is safe as the Future should be !Unpin from the PhantomPinned
        unsafe { self.map_unchecked_mut(|this| &mut this.future).poll(ctx) }
    }
}

impl<F> FutureTask<F> {
    pub fn new(priority: TaskPriority, future: F) -> Self {
        Self {
            pinned: PhantomPinned,
            future,
            task: Task::new(priority, |task| unsafe {
                // To resume: using a ptr to task, find Self via offsetof() and poll() again.
                // Using `uninit()` should not cause UB here since `&raw` doesnt create a ref. 
                let stub = MaybeUninit::<Self>::uninit();
                let base = stub.as_ptr() as usize;
                let field = &raw const (*stub.as_ptr()).task as usize;
                let this = &mut *((task as usize - (field - base)) as *mut Self);
                let _ = this.task.poll(Pin::new_unchecked(&mut this.future));
            })
        }
    }

    pub const from_task(task: Task, future: F) -> Self {
        Self {
            pinned: PhantomPinned,
            future,
            task,
        }
    }
}