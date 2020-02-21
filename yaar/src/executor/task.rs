use core::{
    ptr::null,
    cell::Cell,
};

pub enum Kind {
    Local = 0,
    Global = 1,
}

pub enum Priority {
    Low = 0,
    Normal = 1,
    High = 2,
    Critical = 3,
}

pub struct Task {
    next: Cell<usize>,
    resume: Cell<usize>,
}

impl Task {
    pub fn new(resume: unsafe fn(*const Self)) -> Self {
        Self {
            next: Cell::new(null()),
            resume: Cell::new(resume as usize),
        }
    }

    pub fn kind(&self) -> Kind {
        match self.resume.get() & 1 {
            0 => Kind::Local,
            1 => Kind::Global,
            _ => unreachable!(),
        }
    }

    pub fn priority(&self) -> Priority {
        match self.next.get() & 3 {
            0 => Priority::Low,
            1 => Priority::Normal,
            2 => Priority::High,
            3 => Priority::Critical,
            _ => unreachable!(),
        }
    }
}