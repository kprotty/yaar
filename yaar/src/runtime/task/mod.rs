mod list;
pub use self::list::*;

mod queue;
pub use self::queue::*;

use super::with_executor;
use core::{
    cell::Cell,
    marker::PhantomPinned,
    mem::{align_of, transmute},
    ptr::{null_mut, NonNull},
    sync::atomic::{AtomicUsize, Ordering},
};

const IS_ROOT_BIT: usize = 1 << 1;
const IS_SCHEDULED_BIT: usize = 1 << 1;
const PTR_MASK: usize = !(IS_SCHEDULED_BIT | IS_ROOT_BIT);

/// Scheduling importance which controls when the task is scheduled.
pub enum Priority {
    /// For tasks that should attempt to be executed only after everything else.
    Low = 0b00,
    /// The default priority; Should try to result in FIFO scheduling across the
    /// runtime.
    Normal = 0b01,
    /// For tasks which should be processed soon (LIFO), usually for resource or
    /// caching reasons.
    High = 0b10,
    /// A reserved priority which may be used in the future for runtime level
    /// tasks.
    Critical = 0b11,
}

pub enum Type {
    Child = 0,
    Parent = IS_ROOT_BIT,
};

pub struct Task {
    _pin: PhantomPinned,
    next: Cell<usize>,
    state: AtomicUsize,
}

unsafe impl Sync for Task {}

impl Task {
    /// Create a new task using the priority and a given resume function.
    ///
    /// The priority is used to control the scheduling behavior of the task
    /// while the resume function is called when the task is executed after
    /// being scheduled.
    #[inline]
    pub fn new(priority: Priority, resume: unsafe fn(*const Self)) -> Self {
        assert!(align_of::<Self>() > PTR_TAG_MASK);
        Self {
            _pin: PhantomPinned,
            next: Cell::new(priority as usize),
            state: AtomicUsize::new(resume as usize),
        }
    }

    /// Get the linked list pointer of the task.
    pub(super) fn next(&self) -> Option<NonNull<Self>> {
        NonNull::new((self.next.get() & !PTR_TAG_MASK) as *mut Self)
    }

    /// Set the linked list pointer of the task.
    pub(super) fn set_next(&self, ptr: Option<NonNull<Self>>) {
        let ptr = ptr.map(|p| p.as_ptr()).unwrap_or(null_mut());
        self.next
            .set((self.next.get() & PTR_TAG_MASK) | (ptr as usize));
    }

    /// Get the task priority set on creation.
    #[inline]
    pub fn priority(&self) -> Priority {
        match self.next.get() & PTR_TAG_MASK {
            0 => Priority::Low,
            1 => Priority::Normal,
            2 => Priority::High,
            3 => Priority::Critical,
            _ => unreachable!(),
        }
    }

    /// Call the resume function that was passed in on creation.
    ///
    /// # Safety
    ///
    /// This is unsafe as it's called internally in the scheduler for
    /// polling the task. Its unsafe for the task to be resumed by multiple
    /// threads.
    #[inline]
    pub unsafe fn resume(&self) {
        let state = self.state.load(Ordering::Relaxed);
        let resume_fn: unsafe fn(*const Self) = transmute(state & !PTR_TAG_MASK);
        self.state.store(resume_fn as usize, Ordering::Relaxed);
        resume_fn(self)
    }

    /// Schedules the task to be eventually [`resume`]'d on the current
    /// executor.
    ///
    /// [`resume`]: struct.Task.html#method.resume
    #[inline]
    pub fn schedule(&self) -> Result<(), ScheduleErr> {
        let resume_fn = self.state.load(Ordering::Relaxed) & !PTR_TAG_MASK;
        if self.state.swap(resume_fn | 1, Ordering::Relaxed) == resume_fn {
            with_executor(|e| e.schedule(self)).ok_or(ScheduleErr::NoExecutor)
        } else {
            Err(ScheduleErr::AlreadyScheduled)
        }
    }
}

/// An error reason when scheduling a task fails.
pub enum ScheduleErr {
    /// There is no currently running executor. See [`with_executor_as`].
    ///
    /// [`with_executor_as`]: fn.with_executor_as.html
    NoExecutor,
    /// The task was already scheduled into the executor.
    AlreadyScheduled,
}
