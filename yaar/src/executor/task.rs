use core::{
    cell::Cell,
    mem::{align_of, transmute},
    ptr::{null, NonNull},
};

pub enum Kind {
    Local = 0,
    Global = 1,
}

impl Kind {
    const MASK: usize = 0b1;
}

pub enum Priority {
    Low = 0,
    Normal = 1,
    High = 2,
    Critical = 3,
}

impl Priority {
    const MASK: usize = 0b11;
}

pub struct Task {
    next: Cell<usize>,
    resume: Cell<usize>,
}

impl Task {
    pub fn new(resume: unsafe fn(*const Self)) -> Self {
        assert!(align_of::<Self>() > Priority::MASK.max(Kind::MASK));
        Self {
            next: Cell::new(Priority::Normal as usize),
            resume: Cell::new(resume as usize),
        }
    }

    pub fn next(&self) -> Option<NonNull<Self>> {
        NonNull::new((self.next.get() & !Priority::MASK) as *mut _)
    }

    pub unsafe fn set_next(&self, next_ptr: Option<NonNull<Self>>) {
        let ptr = next_ptr
            .map(|p| p.as_ptr() as usize)
            .unwrap_or(null::<Self>() as usize);
        self.next.set(ptr | (self.priority() as usize));
    }

    pub fn priority(&self) -> Priority {
        match self.next.get() & Priority::MASK {
            0 => Priority::Low,
            1 => Priority::Normal,
            2 => Priority::High,
            3 => Priority::Critical,
            _ => unreachable!(),
        }
    }

    pub unsafe fn resume(&self) {
        let resume_fn = self.resume.get() & !Kind::MASK;
        let resume_fn: unsafe fn(*const Self) = transmute(resume_fn);
        resume_fn(self)
    }

    pub fn kind(&self) -> Kind {
        match self.resume.get() & Kind::MASK {
            0 => Kind::Local,
            1 => Kind::Global,
            _ => unreachable!(),
        }
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
