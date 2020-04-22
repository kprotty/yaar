use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    mem::{align_of, transmute},
    ptr::NonNull,
};

/// Priorities are hints to the scheduler for controlling the order or latency
/// in which tasks are executed on workers. An example use case would be soft
/// back-pressure control for work/task generation.
pub enum Priority {
    /// Hint that the task should be scheduled last or after most other tasks.
    /// In relation to back-pressure, this represents tasks that create work.
    Low = 0,
    /// Hint that the task should be scheduled fairly like any other tasks.
    /// In relation ot back-presure, this represents tasks that either create
    /// work or complete work.
    Normal = 1,
    ///
    High = 2,
    /// Reserved
    Critical = 3,
}

impl Into<usize> for Priority {
    fn into(self) -> usize {
        self as usize
    }
}

impl From<usize> for Priority {
    fn from(value: usize) -> Self {
        match value & 0b11 {
            0 => Self::Low,
            1 => Self::Normal,
            2 => Self::High,
            3 => Self::Critical,
            _ => unreachable!(),
        }
    }
}

pub enum Locality {
    Worker = 0,
    Node = 1,
    Scheduler = 2,
    /// Reserved
    Distributed = 3,
}

impl Into<usize> for Locality {
    fn into(self) -> usize {
        self as usize
    }
}

impl From<usize> for Locality {
    fn from(value: usize) -> Self {
        match value & 0b11 {
            0 => Self::Worker,
            1 => Self::Node,
            2 => Self::Scheduler,
            3 => Self::Distributed,
            _ => unreachable!(),
        }
    }
}

pub struct Task {
    next: UnsafeCell<TaggedPtr<Self, Priority>>,
    resume: UnsafeCell<TaggedPtr<(), Locality>>,
}

unsafe impl Sync for Task {}

impl Task {
    pub fn new(
        priority: Priority,
        locality: Locality,
        resume_fn: unsafe fn(NonNull<Self>),
    ) -> Self {
        let resume_fn = NonNull::new(unsafe { transmute(resume_fn) });
        Self {
            next: UnsafeCell::new(TaggedPtr::new(None, priority)),
            resume: UnsafeCell::new(TaggedPtr::new(resume_fn, locality)),
        }
    }

    pub(crate) fn next(&self) -> Option<NonNull<Self>> {
        unsafe { &*self.next.get() }.ptr()
    }

    pub(crate) unsafe fn set_next(&self, next: Option<NonNull<Self>>) {
        *self.next.get() = TaggedPtr::new(next, self.priority());
    }

    pub fn priority(&self) -> Priority {
        unsafe { &*self.next.get() }.tag()
    }

    pub unsafe fn set_priority(&self, priority: Priority) {
        *self.next.get() = TaggedPtr::new(self.next(), priority);
    }

    pub fn locality(&self) -> Locality {
        unsafe { &*self.resume.get() }.tag()
    }

    pub unsafe fn set_locality(&self, locality: Locality) {
        let ptr = (&*self.resume.get()).ptr();
        *self.resume.get() = TaggedPtr::new(ptr, locality);
    }

    pub unsafe fn resume(&self) {
        use yaar_lock::utils::UnwrapUnchecked;
        let resume_fn = (&*self.resume.get()).ptr().unwrap_unchecked();
        let resume_fn: unsafe fn(NonNull<Self>) = transmute(resume_fn);
        resume_fn(NonNull::from(self));
    }
}

#[derive(Copy, Clone)]
struct TaggedPtr<Ptr, Tag> {
    value: usize,
    _phantom: PhantomData<*mut (Ptr, Tag)>,
}

impl<Ptr, Tag: Into<usize> + From<usize>> TaggedPtr<Ptr, Tag> {
    fn new(ptr: Option<NonNull<Ptr>>, tag: Tag) -> Self {
        assert!(align_of::<Ptr>() >= align_of::<Tag>());
        let ptr = ptr.map(|p| p.as_ptr() as usize).unwrap_or(0);
        Self {
            value: ptr | tag.into(),
            _phantom: PhantomData,
        }
    }

    fn ptr(&self) -> Option<NonNull<Ptr>> {
        NonNull::new((self.value & !(align_of::<Ptr>() - 1)) as *mut Ptr)
    }

    fn tag(&self) -> Tag {
        Tag::from(self.value)
    }
}
