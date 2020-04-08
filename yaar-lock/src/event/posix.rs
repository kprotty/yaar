use crate::event::{OsDuration, OsInstant, time::posix_thread_local};
use core::{
    fmt,
    convert::TryInto,
    cell::{Cell, UnsafeCell},
    mem::MaybeUninit,
    sync::atomic::{Ordering, AtomicUsize},
};
use yaar_sys::{
    timespec,
    pthread_mutex_t,
    PTHREAD_MUTEX_INITIALIZER,
    pthread_mutex_lock,
    pthread_mutex_unlock,
    pthread_mutex_destroy,
    pthread_cond_t,
    pthread_cond_init,
    pthread_cond_signal,
    pthread_cond_wait,
    pthread_cond_timedwait,
    pthread_cond_destroy,
};

#[derive(Copy, Clone, Eq, PartialEq)]
enum State {
    Empty = 0,
    Waiting = 1,
    Notified = 2,
}

#[repr(align(2))]
struct OsSignal {
    state: Cell<State>,
    mutex: UnsafeCell<pthread_mutex_t>,
    condvar: Cell<MaybeUninit<pthread_cond_t>>,
}

impl Drop for OsSignal {
    fn drop(&mut self) {
        unsafe {
            let status_mutex = pthread_mutex_destroy(self.mutex.get());
            let status_condvar = pthread_cond_destroy(self.condvar());

            if cfg!(target_os = "dragonfly") {
                debug_assert!(status_mutex == 0 || status_mutex == yaar_sys::EINVAL);
                debug_assert!(status_condvar == 0 || status_condvar == yaar_sys::EINVAL);
            } else {
                debug_assert_eq!(status_mutex, 0);
                debug_assert_eq!(status_condvar, 0);
            }
        }
    }
}

impl OsSignal {
    fn get<'a>() -> &'a Self {
        unsafe {
            &*posix_thread_local::<Self>(|ptr| {
                #[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))] {
                    use yaar_sys::PTHREAD_COND_INITIALIZER;
                    *ptr = Self {
                        state: Cell::new(State::Empty),
                        mutex: UnsafeCell::new(PTHREAD_MUTEX_INITIALIZER),
                        condvar: Cell::new(MaybeUninit::new(PTHREAD_COND_INITIALIZER)),
                    };
                }
                #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))] {
                    use yaar_sys::{
                        CLOCK_MONOTONIC,
                        pthread_condattr_init,
                        pthread_condattr_setclock,
                        pthread_condattr_destroy,
                    };

                    *ptr = Self {
                        state: Cell::new(State::Empty),
                        mutex: UnsafeCell::new(PTHREAD_MUTEX_INITIALIZER),
                        condvar: Cell::new(MaybeUninit::uninit()),
                    };
                    
                    let mut attr = MaybeUninit::uninit();
                    let status = pthread_condattr_init(attr.as_mut_ptr());
                    debug_assert_eq!(status, 0);

                    let status = pthread_condattr_setclock(attr.as_mut_ptr(), CLOCK_MONOTONIC);
                    debug_assert_eq!(status, 0);

                    let status = pthread_cond_init((&*ptr).condvar(), attr.as_ptr());
                    debug_assert_eq!(status, 0);

                    let status = pthread_condattr_destroy(attr.as_mut_ptr());
                    debug_assert_eq!(status, 0);
                }
            })
        }
    }

    fn locked<T>(&self, f: impl FnOnce() -> T) -> T {
        unsafe {
            let status = pthread_mutex_lock(self.mutex.get());
            debug_assert_eq!(status, 0);

            let result = f();

            let status = pthread_mutex_unlock(self.mutex.get());
            debug_assert_eq!(status, 0);

            result
        }
    }

    fn state(&self) -> &'static str {
        match self.locked(|| self.state.get()) {
            State::Empty => "Empty",
            State::Waiting => "Waiting",
            State::Notified => "Notified",
        }
    }

    fn condvar(&self) -> *mut pthread_cond_t {
        unsafe {
            let maybe_condvar = &*self.condvar.as_ptr();
            maybe_condvar.as_ptr() as *mut pthread_cond_t
        }
    }

    fn notify(&self) {
        self.locked(|| unsafe {
            let state = self.state.get();
            if state == State::Notified {
                return;
            }

            self.state.set(State::Notified);
            if state == State::Waiting {
                let status = pthread_cond_signal(self.condvar());
                debug_assert_eq!(status, 0);
            }
        })
    }

    fn try_wait(&self, mut timeout: Option<&mut OsDuration>) -> bool {
        self.locked(|| unsafe {
            let state = self.state.get();
            if state == State::Waiting {
                unreachable!("Multiple threads waiting on same OsSignal");
            }

            if state == State::Empty {
                self.state.set(State::Waiting);
                let times_out = timeout.as_ref().map(|t| OsInstant::now() + **t);

                while self.state.get() == State::Waiting {
                    let timeout_ts = if let (Some(times_out), Some(timeout)) = (times_out, timeout.as_mut()) {
                        let now = OsInstant::now();
                        if times_out <= now {
                            **timeout = OsDuration::new(0, 0);
                            self.state.set(State::Empty);
                            return false;
                        }
                        **timeout = times_out - now;
                        Self::to_ts(**timeout)
                    } else {
                        None
                    }

                    if let Some(ts) = timeout_ts {
                        let status = pthread_cond_timedwait(self.condvar(), self.mutex.get(), &ts);
                        if ts.tv_sec < 0 {
                            debug_assert!(status == 0 || status == yaar_sys::ETIMEDOUT || status == yaar_sys::EINVAL);
                        } else {
                            debug_assert!(status == 0 || status == yaar_sys::ETIMEDOUT);
                        }
                    } else {
                        let status = pthread_cond_wait(self.condvar(), self.mutex.get());
                        debug_assert_eq!(status, 0);
                    }
                }
            }

            self.state.set(State::Empty);
            true
        })
    }

    fn to_ts(timeout: OsDuration) -> Option<timespec> {
        timeout.as_secs().try_into().ok().and_then(|secs| {
            let now = Self::now();
            let mut secs = now.tv_sec.checked_add(secs);
            let mut nsecs = now.tv_nsec + timeout.subsec_nanos().try_into().unwrap();
            if nsecs >= 1_000_000_000 {
                secs = secs.and_then(|s| s.checked_add(nsecs / 1_000_000_000));
                nsecs = nsecs % 1_000_000_000;
            }
            secs.map(|secs| timespec {
                tv_sec: secs,
                tv_nsec: nsecs,
            })
        })
    }

    unsafe fn now() -> timespec {
        let mut now = MaybeUninit::uninit();

        #[cfg(any(target_os = "macos", target_os = "ios"))] {
            let status = yaar_sys::gettimeofday(now.as_mut_ptr(), null_mut());
            debug_assert_eq!(status, 0);
            let now = now.assume_init();
            timespec {
                tv_sec: now.tv_sec,
                tv_nsec: now.tv_usec.try_into().map(|us| us * 1000).unwrap(),
            }
        }
        
        #[cfg(not(any(target_os = "macos", target_os = "ios")))] {
            let clock = if cfg!(target_os = "android") {
                yaar_sys::CLOCK_REALTIME
            } else {
                yaar_sys::CLOCK_MONOTONIC
            };
            let status = yaar_sys::clock_gettime(clock, now.as_mut_ptr());
            debug_assert_eq!(status, 0);
            now.assume_init()
        }
    }
}

pub struct Signal {
    state: AtomicUsize,
}

impl fmt::Debug for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.load(Ordering::Relaxed);
        let state = if state == State::Empty as usize {
            "Empty"
        } else if state == State::Notified as usize {
            "Notified"
        } else {
            let os_signal = unsafe { &*(state as *const OsSignal) };
            os_signal.state()
        };

        f.debug_struct("OsSignal")
            .field("state", &state)
            .finish()
    }
}

impl Signal {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(State::Empty as usize),
        }
    }

    pub fn notify(&self) {
        let state = self.state.load(Ordering::Acquire);
        if state == State::Empty as usize {
            state = self.state.compare_and_swap(
                State::Empty as usize,
                State::Notified as usize,
                Ordering::Acquire,
            );
            if state == State::Empty as usize {
                return;
            }
        }

        let os_signal = unsafe { &*(state as *const OsSignal) };
        os_signal.notify();
    }

    pub fn wait(&self, timeout: Option<&mut OsDuration>) -> bool {
        let os_signal = OsSignal::get();

        let mut state = self.state.load(Ordering::Relaxed);
        if state == State::Empty as usize {
            state = self.state.compare_and_swap(
                State::Empty as usize,
                os_signal as *const _ as usize,
                Ordering::Release,
            );
        }

        if state == State::Notified as usize {
            self.state.store(State::Empty as usize, Ordering::Relaxed);
            true
        } else {
            os_signal.try_wait(timeout)
        }
    }
}

