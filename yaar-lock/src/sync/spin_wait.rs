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
        let SpinConfig { max_spin, back_off } = SpinConfig::new();
        if self.spin <= max_spin {
            self.spin += 1;
            if back_off {
                (0..(1 << self.spin)).for_each(|_| spin_loop_hint());
            } else {
                spin_loop_hint();
            }
            true
        } else {
            false
        }
    }
}

struct SpinConfig {
    /// Maximum amount of timers SpinWait can spin
    max_spin: usize,
    // True if each SpinWait iteration should exponentially backoff
    back_off: bool,
}

impl SpinConfig {
    /// For non-x86 platforms, its best to not assume what strategy is effective for now
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    fn new() -> Self {
        Self {
            max_spin: 30,
            back_off: false,
        }
    }

    /// For non-Windows x86 platforms, i've observed that backoff spinning is best
    #[cfg(all(not(windows), any(target_arch = "x86", target_arch = "x86_64")))]
    fn new() -> Self {
        Self {
            max_spin: 6,
            back_off: true,
        }
    }

    /// For Windows x86, it appears to be a bit trickier..
    ///
    /// On newer AMD Ryzen CPUs, blocking on the ThreadEvent actually performs better
    /// compared to spinning on the CPU while the opposite seems true for Intel CPUs.
    #[cfg(all(windows, any(target_arch = "x86", target_arch = "x86_64")))]
    fn new() -> Self {
        #[cfg(target_arch = "x86")]
        use core::arch::x86::{__cpuid, CpuidResult};
        #[cfg(target_arch = "x86_64")]
        use core::arch::x86_64::{__cpuid, CpuidResult};

        use core::{
            slice::from_raw_parts,
            str::from_utf8_unchecked,
            hint::unreachable_unchecked,
            sync::atomic::{AtomicUsize, Ordering},
        };

        static IS_AMD: AtomicUsize = AtomicUsize::new(0);
        let is_amd = unsafe {
            match IS_AMD.load(Ordering::Relaxed) {
                0 => {
                    let CpuidResult { ebx, ecx, edx, .. } = __cpuid(0);
                    let vendor = &[ebx, edx, ecx] as *const _ as *const u8;
                    let vendor = from_utf8_unchecked(from_raw_parts(vendor, 3 * 4));
                    let is_amd = vendor == "AuthenticAMD";
                    IS_AMD.store((is_amd as usize) + 1, Ordering::Relaxed);
                    is_amd
                },
                1 => false,
                2 => true,
                _ => unreachable_unchecked(),
            }
        };

        if is_amd {
            Self {
                max_spin: 0,
                back_off: false,
            }
        } else {
            Self {
                max_spin: 6,
                back_off: true,
            }
        }
    }
}