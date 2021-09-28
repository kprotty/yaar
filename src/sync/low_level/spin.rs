use std::{hint::spin_loop, thread};

#[derive(Default)]
pub struct Spin {
    counter: u8,
}

impl Spin {
    pub fn yield_now(&mut self) -> bool {
        if self.counter >= 10 {
            return false;
        }

        if self.counter <= 3 {
            (0..(1 << self.counter)).for_each(|_| spin_loop())
        } else {
            thread::yield_now();
        }

        self.counter += 1;
        true
    }
}
