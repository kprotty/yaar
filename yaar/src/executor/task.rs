use core::{
    cell::Cell,
    hint::unreachable_unchecked,
    marker::PhantomData,
    mem::{align_of, transmute},
    ptr::{null, NonNull},
};

pub(crate) struct TaggedPtr<Ptr, Tag> {
    value: usize,
    phantom: PhantomData<*mut (Ptr, Tag)>,
}

impl<Ptr, Tag> Copy for TaggedPtr<Ptr, Tag> {}
impl<Ptr, Tag> Clone for TaggedPtr<Ptr, Tag> {
    fn clone(&self) -> Self {
        Self::from(self.value)
    }
}

impl<Ptr, Tag> Into<usize> for TaggedPtr<Ptr, Tag> {
    fn into(self) -> usize {
        self.value
    }
}

impl<Ptr, Tag> From<usize> for TaggedPtr<Ptr, Tag> {
    fn from(value: usize) -> Self {
        Self {
            value,
            phantom: PhantomData,
        }
    }
}

impl<Ptr, Tag> TaggedPtr<Ptr, Tag>
where
    Tag: From<usize> + Into<usize>,
{
    pub fn new(ptr: *const Ptr, tag: Tag) -> Self {
        let tag = tag.into();
        debug_assert!(align_of::<Ptr>() > tag);
        ((ptr as usize) | tag).into()
    }

    pub fn as_ptr(self) -> Option<NonNull<Ptr>> {
        NonNull::new((self.value & !(align_of::<Ptr>() - 1)) as *mut Ptr)
    }

    pub fn as_tag(self) -> Tag {
        Tag::from(self.value & (align_of::<Ptr>() - 1))
    }

    pub fn with_ptr(self, ptr: Option<NonNull<Ptr>>) -> Self {
        let ptr = ptr.map(|p| p.as_ptr() as *const _).unwrap_or(null());
        Self::new(ptr, self.as_tag())
    }

    pub fn with_tag(self, tag: Tag) -> Self {
        let ptr = self
            .as_ptr()
            .map(|p| p.as_ptr() as *const _)
            .unwrap_or(null());
        Self::new(ptr, tag)
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Kind {
    Local = 0,
    Global = 1,
}

impl Into<usize> for Kind {
    fn into(self) -> usize {
        self as usize
    }
}

impl From<usize> for Kind {
    fn from(value: usize) -> Self {
        match value & 1 {
            0 => Kind::Local,
            1 => Kind::Global,
            _ => unreachable!(),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Priority {
    Low = 0,
    Normal = 1,
    High = 2,
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
            0 => Priority::Low,
            1 => Priority::Normal,
            2 => Priority::High,
            3 => Priority::Critical,
            _ => unreachable!(),
        }
    }
}

pub struct Task {
    next: Cell<TaggedPtr<Self, Priority>>,
    resume: Cell<TaggedPtr<(), Kind>>,
}

impl Task {
    pub fn new(resume: unsafe fn(*const Self)) -> Self {
        Self {
            next: Cell::new(TaggedPtr::new(null(), Priority::Normal)),
            resume: Cell::new(TaggedPtr::new(resume as *const (), Kind::Local)),
        }
    }

    pub fn next(&self) -> Option<NonNull<Self>> {
        self.next.get().as_ptr()
    }

    pub unsafe fn set_next(&self, next_ptr: Option<NonNull<Self>>) {
        self.next.set(self.next.get().with_ptr(next_ptr));
    }

    pub fn priority(&self) -> Priority {
        self.next.get().as_tag()
    }

    pub unsafe fn set_priority(&self, priority: Priority) {
        self.next.set(self.next.get().with_tag(priority));
    }

    pub fn kind(&self) -> Kind {
        self.resume.get().as_tag()
    }

    pub unsafe fn set_kind(&self, kind: Kind) {
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

#[derive(Default)]
pub(crate) struct TaskList {
    head: Option<NonNull<Task>>,
    tail: Option<NonNull<Task>>,
}

impl TaskList {
    pub unsafe fn push_front(&mut self, list: Self) {
        if let Some(mut tail) = self.tail {
            tail.as_mut().set_next(list.head);
        }
        if self.head.is_none() {
            self.head = list.head;
        }
        self.tail = list.tail;
    }

    pub unsafe fn push_back(&mut self, list: Self) {
        if let Some(tail) = list.tail {
            tail.as_ref().set_next(self.head);
        }
        if self.tail.is_none() {
            self.tail = list.tail;
        }
        self.head = list.head;
    }

    pub unsafe fn pop(&mut self) -> Option<NonNull<Task>> {
        self.head.map(|task| {
            self.head = task.as_ref().next();
            if self.head.is_none() {
                self.tail = None;
            }
            task
        })
    }
}

#[derive(Default)]
pub(crate) struct PriorityTaskList {
    pub(crate) front: TaskList,
    pub(crate) back: TaskList,
    pub(crate) size: usize,
}

impl PriorityTaskList {
    #[inline]
    pub fn len(&self) -> usize {
        self.size
    }

    pub unsafe fn push(&mut self, task: NonNull<Task>) {
        let task_ref = task.as_ref();
        task_ref.set_next(None);
        let priority = task_ref.priority();

        let list = TaskList {
            head: Some(task),
            tail: Some(task),
        };

        self.size += 1;
        match priority {
            Priority::Low | Priority::Normal => self.back.push_back(list),
            Priority::High | Priority::Critical => self.front.push_back(list),
        }
    }

    pub unsafe fn pop(&mut self) -> Option<NonNull<Task>> {
        self.front.pop().or_else(|| self.back.pop()).map(|task| {
            self.size -= 1;
            task
        })
    }
}
