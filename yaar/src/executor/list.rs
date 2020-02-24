use super::{Task, TaskPriority};
use core::ptr::NonNull;

#[derive(Default)]
pub struct TaskQueue {
    head: Option<NonNull<Task>>,
    tail: Option<NonNull<Task>>,
}

impl TaskQueue {
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
pub struct TaskList {
    pub(crate) front: TaskQueue,
    pub(crate) back: TaskQueue,
    pub(crate) size: usize,
}

impl TaskList {
    #[inline]
    pub fn len(&self) -> usize {
        self.size
    }

    pub unsafe fn push(&mut self, task: NonNull<Task>) {
        let task_ref = task.as_ref();
        task_ref.set_next(None);
        let priority = task_ref.priority();

        let list = TaskQueue {
            head: Some(task),
            tail: Some(task),
        };

        self.size += 1;
        match priority {
            TaskPriority::Low | TaskPriority::Normal => self.back.push_back(list),
            TaskPriority::High | TaskPriority::Critical => self.front.push_back(list),
        }
    }

    pub unsafe fn pop(&mut self) -> Option<NonNull<Task>> {
        self.front.pop().or_else(|| self.back.pop()).map(|task| {
            self.size -= 1;
            task
        })
    }
}