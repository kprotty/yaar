mod list;
pub use self::list::*;

mod queue;
pub use self::queue::*;

use core::{
    cell::Cell,
    marker::PhantomPinned,
    mem::{align_of, transmute},
    ptr::{null_mut, NonNull},
};

const PTR_MASK: usize = !0b11;

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

/// Specifies the heirarchy and distribution of a task into the scheduler.
pub enum Kind {
    /// The task should be scheduled in relation to the current parent task.
    Child = 0b00,
    /// The task is independent of others and can be distributed across the
    /// system for scheduling purposes.
    Parent = 0b01,
}

/// A structure which represents a unit of execution for the scheduler.
///
/// A task is akin to the state for a Thread, Fiber, or Coroutine but
/// is small and generic which allows it to be used to execute anything
/// that can provide a valid address for it, not just futures.
pub struct Task {
    _pin: PhantomPinned,
    next: Cell<usize>,
    state: Cell<usize>,
}

unsafe impl Sync for Task {}

impl Task {
    /// Create a new task using the given resume function.
    ///
    /// The resume function is called when the task is executed after
    /// being scheduled.
    #[inline]
    pub fn new(resume: unsafe fn(*const Self)) -> Self {
        assert!(align_of::<Self>() > !PTR_MASK);
        Self {
            _pin: PhantomPinned,
            next: Cell::new(Priority::Normal as usize),
            state: Cell::new((resume as usize) | (Kind::Parent as usize)),
        }
    }

    /// Get the linked list pointer of the task.
    #[inline]
    pub fn next(&self) -> Option<NonNull<Self>> {
        NonNull::new((self.next.get() & PTR_MASK) as *mut Self)
    }

    /// Set the linked list pointer of the task.
    pub fn set_next(&self, ptr: Option<NonNull<Self>>) {
        let ptr = ptr.map(|p| p.as_ptr()).unwrap_or(null_mut());
        self.next
            .set((self.next.get() & !PTR_MASK) | (ptr as usize));
    }

    /// Get the task priority.
    #[inline]
    pub fn priority(&self) -> Priority {
        match self.next.get() & !PTR_MASK {
            0 => Priority::Low,
            1 => Priority::Normal,
            2 => Priority::High,
            3 => Priority::Critical,
            _ => unreachable!(),
        }
    }

    /// Set the priority of the task.
    #[inline]
    pub unsafe fn set_priority(&self, priority: Priority) {
        self.next
            .set((self.next.get() & PTR_MASK) | (priority as usize));
    }

    // Get the task kind.
    #[inline]
    pub fn kind(&self) -> Kind {
        match self.state.get() & 1 {
            0 => Kind::Child,
            1 => Kind::Parent,
            _ => unreachable!(),
        }
    }

    /// Set the task kind.
    pub unsafe fn set_kind(&self, kind: Kind) {
        self.state
            .set((self.state.get() & PTR_MASK) | (kind as usize));
    }

    /// Call the resume function that was passed in on creation.
    ///
    /// # Safety
    ///
    /// This is unsafe as it may be called in parallel inside the scheduler.
    /// The caller should ensure that only one thread is calling a task's resume
    /// fn.
    #[inline]
    pub unsafe fn resume(&self) {
        let resume_fn: unsafe fn(*const Self) = transmute(self.state.get());
        resume_fn(self)
    }
}
