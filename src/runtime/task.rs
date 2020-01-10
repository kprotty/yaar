use core::{
    pin::Pin,
    ptr::NonNull,
    future::Future,
    task::{Poll, Context},
    hint::unreachable_unchecked,
    mem::{align_of, MaybeUninit},
};

pub enum Priority {
    Low = 0,
    Normal = 1,
    High = 2,
}

impl Priority {
    const MASK: usize = 3;
}

pub struct Task {
    data: usize,
}

impl Task {
    pub fn new(priority: Priority, next: Option<NonNull<Self>>) -> Self {
        assert!(align_of::<Self>() > Priority::MASK);
        let ptr = next.map(|ptr| ptr.as_ptr() as usize).unwrap_or(0);
        Self { data: (priority as usize) | ptr }
    }

    pub fn set_next(&mut self, next: Option<NonNull<Self>>) {
        *self = Self::new(self.get_priority(), next);
    }

    pub fn set_priority(&mut self, priority: Priority) {
        *self = Self::new(priority, self.get_next());
    }

    pub fn get_next(self) -> Option<NonNull<Self>> {
        NonNull::new((self.data & !Priority::MASK) as *mut Self)
    }

    pub fn get_priority(self) -> Priority {
        match self.data & Priority::MASK {
            0 => Priority::Low,
            1 => Priority::Normal,
            2 => Priority::High,
            _ => unsafe { unreachable_unchecked() }
        }
    }
}

#[derive(Default)]
pub struct TaskPriorityList {
    front: TaskList,
    back: TaskList,
    size: usize,
}

impl TaskPriorityList {
    pub fn pop(&mut self) -> Option<NonNull<Task>> {
        self.front.pop()
            .or_else(|| self.back.pop())
            .map(|task| {
                self.size -= 1;
                task
            })
    }

    pub fn push(&mut self, task: &mut Task) {
        self.size += 1;
        let list = TaskList {
            head: NonNull::new(task),
            tail: NonNull::new(task),
        };
        match task.get_priority() {
            Priority::Low,
            Priority::Normal => self.back.push(&list),
            Priority::High => self.front.push(list),
        }
    }
}

#[derive(Default)]
pub struct TaskList {
    head: Option<NonNull<Task>>,
    tail: Option<NonNull<Task>>,
}

impl TaskList {
    pub fn pop(&mut self) -> Option<NonNull<Task>> {
        self.head.map(|task| {
            unsafe { self.head = task.as_mut().get_next() };
            if self.head.is_none() {
                self.tail = None;
            }
            task
        })
    }

    pub fn push(&mut self, priority_list: &TaskPriorityList) {
        self.push_front(&priority_list.front);
        self.push_back(&priority_list.back);
    }

    pub fn push_front(&mut self, list: &Self) {
        if let Some(tail) = list.tail {
            unsafe { tail.as_mut().set_next(self.head) };
        }
        if self.tail.is_none() {
            self.tail = list.tail;
        }
        self.head = list.head;
    }

    pub fn push_back(&mut self, list: &Self) {
        if let Some(tail) = self.tail {
            unsafe { tail.as_mut().set_next(list.head) };
        }
        if self.head.is_none() {
            self.head = list.head;
        }
        self.tail = list.tail;
    }
}
