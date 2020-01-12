use super::executor::with_executor;
use core::{
    future::Future,
    hint::unreachable_unchecked,
    marker::Unpin,
    mem::{align_of, MaybeUninit},
    pin::Pin,
    ptr::{null, NonNull},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

/// The importance of a task which
/// may be used to influence scheduling.
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

    /// Call the resume function provided by [`new`]
    /// using the given task as a parameter.
    pub fn resume(&mut self) {
        unsafe { (self.resume)(self) }
    }
}

#[doc(hidden)]
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

#[doc(hidden)]
#[derive(Default)]
pub(super) struct PriorityList {
    front: List,
    back: List,
    pub size: usize,
}

impl PriorityList {
    /*
    pub fn pop<'a>(&mut self) -> Option<&'a mut Task> {
        self.front.pop().or_else(|| self.back.pop()).map(|task| {
            self.size -= 1;
            task
        })
    }
    */

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

/// A combination of a task and a given future
/// which implements [`Task`] based waking and polling.
///
/// For convenience purposes in regards to the executors,
/// the task also stores and caches the output of the future.
pub struct FutureTask<F: Future> {
    future: F,
    pub task: Task,
    output: Option<F::Output>,
}

// TODO: is this safe? as I understand it, the FutureTask
//       shouldnt be moved in memory considering it uses
//       the task pointer in order to compute its reference.
impl<F: Future> Unpin for FutureTask<F> {}

impl<F: Future> Future for FutureTask<F> {
    type Output = NonNull<F::Output>;

    /// Polls the FutureTask for the result or returns the cached output as a NonNull pointer.
    /// This returns a pointer instead of an immutable reference since rust doesnt allow
    /// references for type annotations for [`Future::Output`].
    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        loop {
            // check if the output is already cached
            if let Some(output) = this.output.as_mut() {
                return Poll::Ready(unsafe { NonNull::new_unchecked(output) });
            }

            let pinned = unsafe { Pin::new_unchecked(&mut this.future) };
            match pinned.poll(ctx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(output) => {
                    this.output = Some(output);
                    continue;
                }
            }
        }
    }
}

impl<F: Future> FutureTask<F> {
    /// Create a new future + task wrapper.
    pub fn new(priority: Priority, future: F) -> Self {
        Self {
            future,
            task: Task::new(priority, Self::on_resume),
            output: None,
        }
    }

    /// Convert the FutureTask into its underlying output value.
    pub fn into_output(self) -> Option<F::Output> {
        self.output
    }

    unsafe fn on_resume(task: *mut Task) {
        // Given a pointer to a task, get it's corresponding FutureTask.
        // This uses a `MaybeUninit::zeroed()` instance in order to
        // compute the task field offset but should be replaced if this is merged:
        // https://internals.rust-lang.org/t/pre-rfc-add-a-new-offset-of-macro-to-core-mem/9273
        let this = {
            let stub = MaybeUninit::<Self>::zeroed();
            let base_ptr = stub.as_ptr() as usize;
            let task_ptr = &(&*stub.as_ptr()).task as *const _ as usize;
            &mut *(((task as usize) - (task_ptr - base_ptr)) as *mut Self)
        };
        let _ = this.resume();
    }

    /// Resume the future wrapped task, returning the resulting output.
    ///
    /// This effectively calls `Future::poll()` with the waker and context
    /// setup to resume the future using the task as a reference if the
    /// futures output is not already polled() and stored.
    pub fn resume(&mut self) -> Poll<&F::Output> {
        // check if the output is cached before polling down below
        if self.output.is_some() {
            return Poll::Ready(self.output.as_ref().unwrap());
        }

        const WAKE: unsafe fn(*const ()) =
            |ptr| with_executor(|executor| unsafe { executor.schedule(&mut *(ptr as *mut Task)) });

        const VTABLE: RawWakerVTable =
            RawWakerVTable::new(|ptr| RawWaker::new(ptr, &VTABLE), WAKE, WAKE, |_| {});

        let ptr = &self.task as *const Task as *const ();
        let waker = unsafe { Waker::from_raw(RawWaker::new(ptr, &VTABLE)) };
        let pinned = unsafe { Pin::new_unchecked(self) };

        match pinned.poll(&mut Context::from_waker(&waker)) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(ptr) => Poll::Ready(unsafe { &*ptr.as_ptr() }),
        }
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
