use core::{
    ptr::NonNull,
    mem::{align_of, transmute},
    hint::unreachable_unchecked,
};

pub enum Priority {
    Low = 0b00,
    Normal = 0b01,
    High = 0b10,
}

impl Priority {
    const MASK: usize = 0b11;
}

pub struct Task {
    value: usize
}

impl Task {
    pub fn new(priority: Priority, next: Option<NonNull<Task>>) -> Self {
        assert!(align_of::<Task>() >= Priority::MASK as usize);
        let value = (priority as usize) | unsafe { transmute(next) };
        Self { value }
    }

    pub fn set_next(&mut self, next: Option<NonNull<Task>>) {
        self.value = (self.value & Priority::MASK) | unsafe { transmute(next) };
    }

    pub fn set_priority(&mut self, priority: Priority) {
        self.value = (self.value & !Priority::MASK) | (priority as usize);
    }

    pub fn get_next(&self) -> Option<NonNull<Task>> {
        unsafe { transmute(self.value & Priority::MASK) }
    }

    pub fn get_priority(&self) -> Priority {
        match self.value & Priority::MASK {
            Priority::Low as usize => Priority::Low,
            Priority::Normal as usize => Priority::Normal,
            Priority::High as usize => Priority::High,
            _ => unsafe { unreachable_unchecked() },
        }
    }
}

#[derive(Default, Copy, Clone)]
pub struct PriorityList {
    front: List,
    back: List,
    size: usize,
}

impl PriorityList {
    pub fn pop(&mut self) -> Option<NonNull<Task>> {
        self.front.pop().or_else(|| self.back.pop())
    }

    pub fn push(&mut self, task: &mut Task) {
        self.size += 1;
        let list = List::new(NonNull::new(task));
        match task.get_priority() {
            Priority::Low, 
            Priority::Normal => self.back.push(list),
            Priority::High => self.front.push(list),
        }
    }
}

#[derive(Copy, Clone)]
pub struct List {
    head: Option<NonNull<Task>>,
    tail: Option<NonNull<Task>>,
}

impl Default for List {
    fn default() -> Self {
        Self::new(None)
    }
}

impl List {
    pub fn new(top: Option<NonNull<Task>>) -> Self {
        Self {
            head: top,
            tail: top,
        }
    }

    pub fn pop(&mut self) -> Option<NonNull<Task>> {
        self.head.clone().and_then(|head| {
            self.head = head.get_next();
            if self.head.is_none() {
                self.tail = None;
            }
            head
        })
    }

    pub fn push_list(&mut self, priority_list: PriorityList) {
        self.push_front(priority_list.front);
        self.push_back(priority_list.back);
    }

    pub fn push_front(&mut self, list: List) {
        if let Some(tail) = list.tail {
            tail.set_next(self.head);
        }
        if self.tail.is_none() {
            self.tail = list.tail;
        }
        self.head = list.head;
    }

    pub fn push_back(&mut self, list: List) {
        if let Some(tail) = self.tail {
            tail.set_next(list.head);
        }
        if self.head.is_none() {
            self.head = list.head;
        }
        self.tail = list.tail;
    }
}