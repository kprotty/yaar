use super::TaggedPtr;
use core::{
    cell::Cell,
    hint::unreachable_unchecked,
    mem::transmute,
    ptr::{null, NonNull},
};

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum TaskScope {
    /// Distribute the task to the current worker
    Local = 0,
    /// Distribute the task to workers on the same node
    Global = 1,
    /// Distribute the task to workers on other nodes
    System = 2,
    /// Distribute the task to workers on nodes outside of the system
    Remote = 3,
}

impl Into<usize> for TaskScope {
    fn into(self) -> usize {
        self as usize
    }
}

impl From<usize> for TaskScope {
    fn from(value: usize) -> Self {
        match value & 1 {
            0 => TaskScope::Local,
            1 => TaskScope::Global,
            2 => TaskScope::System,
            3 => TaskScope::Remote,
            _ => unreachable!(),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum TaskPriority {
    Low = 0,
    Normal = 1,
    High = 2,
    Critical = 3,
}

impl Into<usize> for TaskPriority {
    fn into(self) -> usize {
        self as usize
    }
}

impl From<usize> for TaskPriority {
    fn from(value: usize) -> Self {
        match value & 0b11 {
            0 => TaskPriority::Low,
            1 => TaskPriority::Normal,
            2 => TaskPriority::High,
            3 => TaskPriority::Critical,
            _ => unreachable!(),
        }
    }
}

pub struct Task {
    next: Cell<TaggedPtr<Self, TaskPriority>>,
    resume: Cell<TaggedPtr<(), TaskScope>>,
}

impl Task {
    pub fn new(resume: unsafe fn(*const Self)) -> Self {
        Self {
            next: Cell::new(TaggedPtr::new(null(), TaskPriority::Normal)),
            resume: Cell::new(TaggedPtr::new(resume as *const (), TaskScope::Local)),
        }
    }

    pub fn next(&self) -> Option<NonNull<Self>> {
        self.next.get().as_ptr()
    }

    pub unsafe fn set_next(&self, next_ptr: Option<NonNull<Self>>) {
        self.next.set(self.next.get().with_ptr(next_ptr));
    }

    pub fn priority(&self) -> TaskPriority {
        self.next.get().as_tag()
    }

    pub unsafe fn set_priority(&self, priority: TaskPriority) {
        self.next.set(self.next.get().with_tag(priority));
    }

    pub fn scope(&self) -> TaskScope {
        self.resume.get().as_tag()
    }

    pub unsafe fn set_scope(&self, kind: TaskScope) {
        self.resume.set(self.resume.get().with_tag(kind));
    }

    pub unsafe fn resume(&self) {
        let resume_fn: unsafe fn(*const Self) = transmute(
            self.resume
                .get()
                .as_ptr()
                .unwrap_or_else(|| unreachable_unchecked())
                .as_ptr(),
        );
        resume_fn(self)
    }
}
