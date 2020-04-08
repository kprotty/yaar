use super::OsDuration;
use core::{
    ptr::{drop_in_place, NonNull},
    convert::TryInto,
    mem::{size_of, align_of, MaybeUninit},
};
use yaar_sys::{
    malloc,
    free,
    pthread_once_t,
    PTHREAD_ONCE_INIT,
    pthread_once,
    pthread_key_t,
    pthread_key_create,
    pthread_getspecific,
    pthread_setspecific,
};

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub unsafe fn os_timestamp() -> OsDuration {
    #[derive(Copy, Clone)]
    struct mach_timebase_info {
        numer: u32,
        denom: u32,
    }
    extern "C" {
        fn mach_absolute_time() -> u64;
        fn mach_timebase_info(info: *mut mach_timebase_info) -> yaar_sys::c_int;
    }

    let info = {
        const UNINIT: usize = 0;
        const CREATING: usize = 1;
        const INIT: usize = 2;

        use core::sync::atomic::{Ordering, AtomicUsize};
        static STATE: AtomicUsize = AtomicUsize::new(0);
        static mut INFO: MaybeUninit<mach_timebase_info> = MaybeUninit::uninit();

        if STATE.load(Ordering::Acquire) == INIT {
            INFO.assume_init()
        } else {
            let mut info = MaybeUninit::uninit();
            let status = mach_timebase_info(info.as_mut_ptr());
            debug_assert_eq!(status, 0);
            if STATE.compare_and_swap(UNINIT, CREATING, Ordering::Relaxed) == UNINIT {
                INFO = info;
                STATE.store(INIT, Ordering::Release);
            }
            info.assume_init()
        }
    };

    const NANOS_PER_SEC: u64 = 1_000_000_000;
    let now = mach_absolute_time();
    let nanos = (now * info.numer) / info.denom;
    OsDuration::new(
        nanos / NANOS_PER_SEC,
        (nanos % NANOS_PER_SEC) as u32,
    )
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub unsafe fn os_timestamp() -> OsDuration {
    use yaar_sys::{timespec, CLOCK_MONOTONIC, clock_gettime};

    let mut now_ts = MaybeUninit::uninit();
    let status = clock_gettime(CLOCK_MONOTONIC, now_ts.as_mut_ptr());
    debug_assert_eq!(status, 0);
    let mut now_ts = now_ts.assume_init();

    let actually_monotonic = 
        cfg!(all(target_os = "linux", target_arch = "x86_64")) ||
        cfg!(all(target_os = "linux", target_arch = "x86")) ||
        cfg!(target_os = "fuchsia");

    if !actually_monotonic {
        let current_ts = &mut *posix_thread_local(|ptr| {
            *ptr = timespec { tv_sec: 0, tv_nsec: 0 };
        });
        let went_backwards = if current_ts.tv_sec == now_ts.tv_sec {
            current_ts.tv_nsec > now_ts.tv_nsec 
        } else {
            current_ts.tv_sec > now_ts.tv_sec
        };
        if went_backwards {
            now_ts = *current_ts;
        } else {
            *current_ts = now_ts;
        }
    }

    OsDuration::new(
        now_ts.tv_sec.try_into().unwrap(),
        now_ts.tv_nsec.try_into().unwrap(),
    )
}

pub(crate) unsafe fn posix_thread_local<T: Sized>(
    init: impl FnOnce(*mut T),
) -> *mut T {
    static mut ONCE: pthread_once_t = PTHREAD_ONCE_INIT;
    static mut KEY: MaybeUninit<pthread_key_t> = MaybeUninit::uninit();
    
    let status = pthread_once(&mut KEY, || {
        extern "C" fn drop(ptr: *mut yaar_sys::c_void) {
            drop_in_place(ptr as *mut T);
            free(ptr);
        };
        let status = pthread_key_create(KEY.as_mut_ptr(), Some(drop));
        assert_eq!(status, 0, "OS failed to allocate thread local key ");
    });
    debug_assert_eq!(status, 0);

    let key = KEY.assume_init();
    NonNull::new(pthread_getspecific(key) as *mut T)
        .unwrap_or_else(|| {
            let ptr = malloc(size_of::<T>());
            assert!(!ptr.is_null(), "OS failed to allocate thread local memory");
            assert_eq!((ptr as usize) & (align_of::<T>() - 1), 0, "OS malloc() returned unaligned ptr");

            let status = pthread_setspecific(key, ptr);
            assert_eq!(status, 0, "Failed to store thread local memory in thread local");
            init(ptr as *mut T);
            NonNull::new_unchecked(ptr as *mut T)
        })
        .as_ptr()
}
