use super::OsDuration;
use core::{
    cmp,
    mem::{transmute, size_of},
    sync::atomic::{spin_loop_hint, Ordering, AtomicUsize},
};

pub trait Clock {
    const IS_ALWAYS_MONOTONIC: bool;

    fn nanotime() -> u64;
}

pub unsafe trait ThreadLocalStorage {
    fn alloc() -> Option<usize>;

    fn dealloc(key: usize);

    fn get(key: usize) -> usize;

    fn set(key: usize, value: usize);
}

pub unsafe fn timestamp<System: Clock + ThreadLocalStorage>() -> OsDuration {
    const NUM_KEYS: usize = size_of::<u64>() / size_of::<usize>();

    let get_tls_keys = || {
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
                            if let Some(key) = System::alloc() {
                                created += 1;
                                TLS_KEYS[k] = key;
                            } else {
                                for c in 0..created {
                                    System::dealloc(TLS_KEYS[c]);
                                }
                                state = ERROR;
                                STATE.store(state, Ordering::Release);
                                continue 'state;
                            }
                        }
                        state = READY;
                        STATE.store(state, Ordering::Release);
                    }
                },
                CREATING => {
                    spin = spin.wrapping_add(1);
                    (0..(spin % 64)).for_each(|_| spin_loop_hint());
                    state = STATE.load(Ordering::Acquire);
                },
                READY => {
                    break TLS_KEYS
                },
                _ => {
                    unreachable!("Monotonic timer failed to initialize");
                },
            }
        }
    };

    let load_tls = |tls_keys: [usize; NUM_KEYS]| {
        let mut nano_words = [0usize; NUM_KEYS];
        for k in 0..NUM_KEYS {
            nano_words[k] = System::get(tls_keys[k]);
        }
        transmute::<_, u64>(nano_words)
    };

    let store_tls = |tls_keys: [usize; NUM_KEYS], nanos: u64| {
        let nano_words = transmute::<_, [usize; NUM_KEYS]>(nanos);
        for k in 0..NUM_KEYS {
            System::set(tls_keys[k], nano_words[k]);
        }
    };

    let mut now = System::nanotime();

    if !System::IS_ALWAYS_MONOTONIC {
        let tls_keys = get_tls_keys();
        let current = load_tls(tls_keys);
        if current.cmp(&now) == cmp::Ordering::Less {
            store_tls(tls_keys, now);
        } else {
            now = current;
        }
    }

    OsDuration::from_nanos(now)
}
