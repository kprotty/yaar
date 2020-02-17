use core::sync::atomic::spin_loop_hint;

pub struct SpinWait {
    spin: usize,
}

impl SpinWait {
    pub fn new() -> Self {
        Self { spin: 0 }
    }

    pub fn reset(&mut self) {
        self.spin = 0;
    }

    pub fn spin(&mut self) -> bool {
        if self.spin <= 5 {
            self.spin += 1;
            if Self::should_spin() {
                (0..(1 << self.spin)).for_each(|_| spin_loop_hint());
            } else {
                spin_loop_hint();
            }
            true
        } else {
            false
        }
    }

    fn should_spin() -> bool {
        // TODO: Find the system criteria in which yielding to the OS is almost strictly
        // better (This appears most on modern ryzen systems under Windows 10).
        if cfg!(all(windows, feature = "os")) {
            false
        } else {
            true
        }
    }
}
