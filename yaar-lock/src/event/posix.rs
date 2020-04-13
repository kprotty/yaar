use super::{AutoResetEvent, OsAutoResetEvent, OsDuration, OsInstant, YieldContext};
use core::{
    cell::UnsafeCell,
    convert::TryInto,
    fmt,
    marker::PhantomData,
    mem::{align_of, size_of, MaybeUninit},
    ptr::{drop_in_place, write},
    sync::atomic::{AtomicUsize, Ordering},
};
use yaar_sys::{
    c_void, free, malloc, pthread_cond_destroy, pthread_cond_signal, pthread_cond_t,
    pthread_cond_timedwait, pthread_cond_wait, pthread_getspecific, pthread_key_create,
    pthread_key_t, pthread_mutex_destroy, pthread_mutex_lock, pthread_mutex_t,
    pthread_mutex_unlock, pthread_setspecific, time_t, timespec, EINVAL, ETIMEDOUT,
    PTHREAD_COND_INITIALIZER, PTHREAD_MUTEX_INITIALIZER,
};

const EMPTY: usize = 0;
const WAITING: usize = 1;
const NOTIFIED: usize = 2;

pub struct Signal {
    state: AtomicUsize,
}

impl fmt::Debug for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match self.state.load(Ordering::Acquire) {
            EMPTY => "empty",
            NOTIFIED => "notified",
            ptr => unsafe { (&*(ptr as *const ThreadSignal)).state() },
        };

        f.debug_struct("OsSignal").field("state", &state).finish()
    }
}

impl Signal {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(EMPTY),
        }
    }

    pub fn notify(&self) {
        let mut state = self.state.load(Ordering::Acquire);
        if state == EMPTY {
            state = self
                .state
                .compare_and_swap(EMPTY, NOTIFIED, Ordering::AcqRel);
            if state == EMPTY {
                return;
            }
        }

        if state != NOTIFIED {
            let thread_signal = unsafe { &*(state as *const ThreadSignal) };
            thread_signal.notify();
        }
    }

    pub fn try_wait(&self, timeout: Option<&OsInstant>) -> bool {
        let mut state = self.state.load(Ordering::Acquire);
        if state == EMPTY {
            let thread_signal_ptr = ThreadSignal::get();
            state =
                self.state
                    .compare_and_swap(EMPTY, thread_signal_ptr as usize, Ordering::AcqRel);
            if state == EMPTY {
                state = thread_signal_ptr as usize;
            }
        }

        if state == NOTIFIED {
            self.state.store(EMPTY, Ordering::Relaxed);
            return true;
        }

        let thread_signal = unsafe { &*(state as *const ThreadSignal) };
        thread_signal.try_wait(timeout)
    }
}

#[repr(align(2))]
struct ThreadSignal {
    state: UnsafeCell<usize>,
    cond: UnsafeCell<pthread_cond_t>,
    mutex: UnsafeCell<pthread_mutex_t>,
}

impl Drop for ThreadSignal {
    fn drop(&mut self) {
        unsafe {
            let status_mutex = pthread_mutex_destroy(self.mutex.get());
            let status_cond = pthread_cond_destroy(self.cond.get());

            if cfg!(target_os = "dragonfly") {
                debug_assert!(status_mutex == 0 || status_mutex == EINVAL);
                debug_assert!(status_cond == 0 || status_cond == EINVAL);
            } else {
                debug_assert_eq!(status_mutex, 0);
                debug_assert_eq!(status_cond, 0);
            }
        }
    }
}

impl ThreadSignal {
    fn locked<T>(&self, f: impl FnOnce(&mut usize) -> T) -> T {
        unsafe {
            let status = pthread_mutex_lock(self.mutex.get());
            debug_assert_eq!(status, 0);

            let result = f(&mut *self.state.get());

            let status = pthread_mutex_unlock(self.mutex.get());
            debug_assert_eq!(status, 0);

            result
        }
    }

    fn state(&self) -> &'static str {
        self.locked(|state| match *state {
            EMPTY => "empty",
            WAITING => "has_waiter",
            NOTIFIED => "notified",
            _ => unreachable!(),
        })
    }

    fn notify(&self) {
        self.locked(|state| unsafe {
            if *state == EMPTY {
                *state = NOTIFIED;
                return;
            }

            debug_assert_eq!(*state, WAITING);
            *state = EMPTY;
            let status = pthread_cond_signal(self.cond.get());
            debug_assert_eq!(status, 0);
        })
    }

    fn try_wait(&self, timeout: Option<&OsInstant>) -> bool {
        self.locked(|state| unsafe {
            if *state == NOTIFIED {
                *state = EMPTY;
                return true;
            }

            *state = WAITING;
            while *state == WAITING {
                if let Some(timeout) = timeout {
                    let now = OsInstant::now();
                    if *timeout <= now {
                        *state = EMPTY;
                        return false;
                    }

                    if let Some(ts) = Self::timeout_to_ts(*timeout - now) {
                        let status = pthread_cond_timedwait(self.cond.get(), self.mutex.get(), &ts);
                        if ts.tv_sec < 0 {
                            debug_assert!(status == 0 || status == ETIMEDOUT || status == EINVAL);
                        } else {
                            debug_assert!(status == 0 || status == ETIMEDOUT);
                        }
                    } else {
                        let status = pthread_cond_wait(self.cond.get(), self.mutex.get());
                        debug_assert_eq!(status, 0);
                    }
                } else {
                    let status = pthread_cond_wait(self.cond.get(), self.mutex.get());
                    debug_assert_eq!(status, 0);
                }
            }

            debug_assert_eq!(*state, EMPTY);
            true
        })
    }

    unsafe fn timeout_to_ts(timeout: OsDuration) -> Option<timespec> {
        // x32 Linux uses a non-standard type for tv_nsec in timespec.
        // See https://sourceware.org/bugzilla/show_bug.cgi?id=16437
        #[cfg(all(target_arch = "x86_64", target_pointer_width = "32"))]
        #[allow(non_camel_case_types)]
        type tv_nsec_t = i64;
        #[cfg(not(all(target_arch = "x86_64", target_pointer_width = "32")))]
        #[allow(non_camel_case_types)]
        type tv_nsec_t = yaar_sys::c_long;

        let secs: time_t = timeout.as_secs().try_into().ok()?;

        let now = Self::ts_now();
        let mut sec = now.tv_sec.checked_add(secs)?;
        let mut nsec = now.tv_nsec + timeout.subsec_nanos() as tv_nsec_t;
        if nsec >= 1_000_000_000 {
            sec = sec.checked_add(nsec / 1_000_000_000)?;
            nsec %= 1_000_000_000;
        }

        Some(timespec {
            tv_nsec: nsec,
            tv_sec: sec,
        })
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    unsafe fn ts_now() -> timespec {
        use yaar_sys::{gettimeofday, timeval};

        let mut now = MaybeUninit::uninit();
        let status = gettimeofday(now.as_mut_ptr(), null_mut());
        debug_assert_eq!(status, 0);

        let now = now.assume_init();
        timespec {
            tv_sec: now.tv_sec,
            tv_nsec: now.tv_usec.try_into().unwrap_unchecked() * 1000,
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    unsafe fn ts_now() -> timespec {
        use yaar_sys::{clock_gettime, CLOCK_MONOTONIC, CLOCK_REALTIME};

        let mut now = MaybeUninit::uninit();
        let clock = if cfg!(target_os = "android") {
            CLOCK_REALTIME
        } else {
            CLOCK_MONOTONIC
        };

        let status = clock_gettime(clock, now.as_mut_ptr());
        debug_assert_eq!(status, 0);
        now.assume_init()
    }

    fn get() -> *const Self {
        static SIGNAL: LocalKey<ThreadSignal> = LocalKey::new();

        SIGNAL.get(|ptr| unsafe {
            write(
                ptr,
                Self {
                    state: UnsafeCell::new(EMPTY),
                    cond: UnsafeCell::new(PTHREAD_COND_INITIALIZER),
                    mutex: UnsafeCell::new(PTHREAD_MUTEX_INITIALIZER),
                },
            );

            #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))]
            {
                use yaar_sys::{
                    pthread_cond_init, pthread_condattr_destroy, pthread_condattr_init,
                    pthread_condattr_setclock, CLOCK_MONOTONIC,
                };

                let mut attr = MaybeUninit::uninit();
                let status = pthread_condattr_init(attr.as_mut_ptr());
                debug_assert_eq!(status, 0);

                let status = pthread_condattr_setclock(attr.as_mut_ptr(), CLOCK_MONOTONIC);
                debug_assert_eq!(status, 0);

                let status = pthread_cond_init((&*ptr).cond.get(), attr.as_ptr());
                debug_assert_eq!(status, 0);

                let status = pthread_condattr_destroy(attr.as_mut_ptr());
                debug_assert_eq!(status, 0);
            }
        })
    }
}

pub struct LocalKey<T> {
    state: AtomicUsize,
    key: MaybeUninit<pthread_key_t>,
    _phantom: PhantomData<T>,
}

unsafe impl<T> Sync for LocalKey<T> {}

impl<T: Sized> LocalKey<T> {
    const UNINIT: usize = 0;
    const CREATING: usize = 1;
    const READY: usize = 2;
    const ERROR: usize = 3;

    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(Self::UNINIT),
            key: MaybeUninit::uninit(),
            _phantom: PhantomData,
        }
    }

    pub fn get(&self, init: impl FnOnce(*mut T)) -> *mut T {
        unsafe {
            let key = {
                if self.state.load(Ordering::Acquire) != Self::READY {
                    self.create_key();
                }
                self.key.assume_init()
            };

            let mut ptr = pthread_getspecific(key) as *mut T;
            if ptr.is_null() {
                ptr = self.alloc_value(key, init);
            }

            ptr as *mut T
        }
    }

    unsafe extern "C" fn destructor(ptr: *mut c_void) {
        drop_in_place(ptr as *mut T);
        free(ptr);
    }

    #[cold]
    unsafe fn create_key(&self) {
        let mut spin: usize = 0;
        let mut state = self.state.load(Ordering::Acquire);

        while state != Self::READY {
            match state {
                // Try to initialize the thread local key by acquiring the CREATING lock.
                // Limit the pthread_key_create to one thread to avoid key starvation elsewhere.
                Self::UNINIT => {
                    state = self
                        .state
                        .compare_and_swap(state, Self::CREATING, Ordering::AcqRel);
                    if state == Self::UNINIT {
                        let status =
                            pthread_key_create(self.key.as_ptr() as *mut _, Some(Self::destructor));
                        state = match status {
                            0 => Self::READY,
                            _ => Self::ERROR,
                        };
                        self.state.store(state, Ordering::Release);
                    }
                }
                // Another thread is busy creating the thread local key.
                // Spin on the state until the thread is ready
                Self::CREATING => {
                    if OsAutoResetEvent::yield_now(YieldContext {
                        contended: false,
                        iteration: spin,
                        _sealed: (),
                    }) {
                        spin = spin.wrapping_add(1);
                    } else {
                        spin = 0;
                    }
                    state = self.state.load(Ordering::Acquire);
                }
                //
                Self::READY => return,
                _ => unreachable!("OS failed to allocate thread local key"),
            }
        }
    }

    #[cold]
    unsafe fn alloc_value(&self, key: pthread_key_t, init: impl FnOnce(*mut T)) -> *mut T {
        let ptr = malloc(size_of::<T>() + align_of::<T>()) as *mut u8;
        assert!(!ptr.is_null(), "OS failed to allocate thread local object");
        let ptr = ptr.add(ptr.align_offset(align_of::<T>())) as *mut T;

        init(ptr);
        let status = pthread_setspecific(key, ptr as *mut _);
        if status != 0 {
            drop_in_place(ptr);
            unreachable!("OS failed to store thread local object in tls key");
        }

        ptr
    }
}
