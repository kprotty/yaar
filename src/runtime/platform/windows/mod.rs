use super::{
    Platform,
    super::utils::Lazy,
};
use core::{
    num::NonZeroUsize,
    mem::{size_of, MaybeUninit},
};
use winapi::um::{
    processthreadsapi::SwitchToThread,
    sysinfoapi::{SYSTEM_INFO, GetSystemInfo},
};

#[derive(Default, Copy, Clone)]
pub struct OsPlatform;

impl Platform for OsPlatform {
    fn yield_now() {
        let _ = unsafe { SwitchToThread() };
    }

    fn min_thread_stack_size() -> usize {
        let system_info = OS_SYSTEM_INFO.get::<Self>().0;
        (system_info.dwPageSize as usize) << size_of::<usize>()
    }

    fn num_cpus() -> Option<NonZeroUsize> {
        let system_info = OS_SYSTEM_INFO.get::<Self>().0;
        NonZeroUsize::new(system_info.dwNumberOfProcessors as usize)
    }
}


struct OsSystemInfo(SYSTEM_INFO);

static OS_SYSTEM_INFO: Lazy<OsSystemInfo> = Lazy::new();

impl Default for OsSystemInfo {
    fn default() -> Self {
        unsafe {
            let mut system_info = MaybeUninit::uninit();
            GetSystemInfo(system_info.as_mut_ptr());
            Self(system_info.assume_init())
        }
    }
}
