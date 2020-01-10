use core::{
    mem::align_of,
    hint::unreachable_unchecked,
};

pub enum Priority {
    Low = 0,
    Normal = 1,
    High = 2,
}

impl Priority {
    const MASK: usize = 3;
}

#[derive(Copy, Clone)]
pub struct Task {
    data: usize,
}

impl Task {
    pub fn new(priority: Priority, next: Option<*mut Self>) -> Self {
        assert!(align_of::<Self>() > Priority::MASK);
        Self {
            data: (priority as usize) | next.map(|ptr| ptr as usize).unwrap_or(0),
        }
    }

    pub fn with_next(self, next: Option<*mut Self>) -> Self {
        Self::new(self.get_priority(), next)
    }

    pub fn with_priority(self, priority: Priority) -> Self {
        Self::new(priority, self.get_next())
    }

    pub fn get_next(self) -> Option<*mut Self> {
        match self.data & !Priority::MASK {
            0 => None,
            ptr => Some(ptr as *mut Self),
        }
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