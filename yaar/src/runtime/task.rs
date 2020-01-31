pub enum Priority {
    Low = 0b00,
    Normal = 0b01,
    High = 0b10,
    Critical = 0b11,
}

impl From<usize> for Priority {
    #[inline]
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
    next: usize,
    resume: unsafe fn(*mut Task),
}

impl Task {
    #[inline]
    pub fn new(priority: Priority, resume: unsafe fn(*mut Task)) -> Self {
        Self {
            resume,
            next: priority as usize,
        }
    }

    #[inline]
    pub fn priority(&self) -> Priority {
        self.next.into()
    }

    #[inline]
    pub unsafe fn resume(&mut self) {
        (self.resume)(self)
    }
}
