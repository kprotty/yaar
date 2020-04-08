use super::OsDuration;
use crate::utils::UnwrapUnchecked;
use core::{
    cmp,
    convert::TryInto,
    mem::{size_of, transmute},
    sync::atomic::{spin_loop_hint, AtomicUsize, Ordering},
};
use yaar_sys::{GetLastError, ERROR_SUCCESS};
use yaar_sys::{QueryPerformanceCounter, QueryPerformanceFrequency, LARGE_INTEGER, TRUE};
use yaar_sys::{TlsAlloc, TlsFree, TlsGetValue, TlsSetValue, TLS_OUT_OF_INDEXES};

pub unsafe fn os_timestamp() -> OsDuration {
    let mut now = nanotime();

    let tls_keys = get_tls_keys();
    let current = load_tls(tls_keys);
    if current.cmp(&now) == cmp::Ordering::Less {
        store_tls(tls_keys, now);
    } else {
        now = current;
    }

    OsDuration::from_nanos(now)
}

const NANOS_PER_SEC: LARGE_INTEGER = 1_000_000_000;
const NANOS_RESOLUTION: LARGE_INTEGER = 100;

unsafe fn nanotime() -> u64 {
    let ticks_per_sec = {
        const UNINIT: usize = 0;
        const CREATING: usize = 1;
        const READY: usize = 2;

        static mut FREQUENCY: LARGE_INTEGER = 0;
        static STATE: AtomicUsize = AtomicUsize::new(UNINIT);

        if STATE.load(Ordering::Acquire) == READY {
            FREQUENCY
        } else {
            let mut frequency = 0;
            let status = QueryPerformanceFrequency(&mut frequency);
            debug_assert_eq!(status, TRUE);

            if STATE.compare_and_swap(UNINIT, CREATING, Ordering::Relaxed) == 0 {
                FREQUENCY = frequency;
                STATE.store(READY, Ordering::Release);
            }

            frequency
        }
    };

    let mut ticks = 0;
    let status = QueryPerformanceCounter(&mut ticks);
    debug_assert_eq!(status, TRUE);

    let resolution = NANOS_PER_SEC / NANOS_RESOLUTION;
    (ticks / (ticks_per_sec / resolution)) as u64
}

const NUM_KEYS: usize = size_of::<u64>() / size_of::<usize>();

unsafe fn load_tls(tls_keys: [usize; NUM_KEYS]) -> LARGE_INTEGER {
    let mut words = [0usize; NUM_KEYS];
    for k in 0..NUM_KEYS {
        words[k] = unsafe {
            let key = tls_keys[k].try_into().unwrap_unchecked();
            let value = TlsGetValue(key);
            debug_assert_eq!(ERROR_SUCCESS, GetLastError());
            value
        };
    }
    transmute(words)
}

unsafe fn store_tls(tls_keys: [usize; NUM_KEYS], value: LARGE_INTEGER) {
    let words = transmute::<_, [usize; NUM_KEYS]>(value);
    for k in 0..NUM_KEYS {
        unsafe {
            let key = tls_keys[k].try_into().unwrap_unchecked();
            let stored = TlsSetValue(key, words[k]);
            debug_assert_eq!(stored, TRUE);
        };
    }
}

unsafe fn get_tls_keys() -> [usize; NUM_KEYS] {
    const UNINIT: usize = 0;
    const CREATING: usize = 1;
    const READY: usize = 2;
    const ERROR: usize = 3;

    static STATE: AtomicUsize = AtomicUsize::new(UNINIT);
    static mut TLS_KEYS: [usize; NUM_KEYS] = [0; NUM_KEYS];

    let mut spin: usize = 0;
    let mut state = STATE.load(Ordering::Acquire);

    'state: loop {
        match state {
            UNINIT => {
                state = STATE.compare_and_swap(UNINIT, CREATING, Ordering::Acquire);
                if state == UNINIT {
                    let mut created = 0;
                    for k in 0..NUM_KEYS {
                        match TlsAlloc() {
                            TLS_OUT_OF_INDEXES => {
                                for c in 0..created {
                                    let key = TLS_KEYS[c].try_into().unwrap_unchecked();
                                    let freed = TlsFree(key);
                                    debug_assert_eq!(freed, TRUE);
                                }
                                state = ERROR;
                                STATE.store(state, Ordering::Release);
                                continue 'state;
                            },
                            key => {
                                created += 1;
                                TLS_KEYS[k] = key;
                            }
                        }
                    }
                    state = READY;
                    STATE.store(state, Ordering::Release);
                }
            }
            CREATING => {
                spin = spin.wrapping_add(1);
                (0..(spin % 64)).for_each(|_| spin_loop_hint());
                state = STATE.load(Ordering::Acquire);
            }
            READY => break TLS_KEYS,
            _ => {
                unreachable!("Monotonic timer failed to initialize");
            }
        }
    }
}