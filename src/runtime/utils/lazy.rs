use super::super::platform;
use core::{
    cell::Cell,
    mem::MaybeUninit,
    hint::unreachable_unchecked,
    sync::atomic::{Ordering, AtomicU8, spin_loop_hint},
};

pub struct Lazy<T> {
    state: AtomicU8,
    value: Cell<MaybeUninit<T>>,
}

const STATE_UNINIT: u8 = 0;
const STATE_LOADING: u8 = 1;
const STATE_INIT: u8 = 2;

unsafe impl<T> Send for Lazy<T> {}
unsafe impl<T> Sync for Lazy<T> {}

impl<T> Lazy<T> {
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(STATE_UNINIT),
            value: Cell::new(MaybeUninit::uninit()),
        }
    }
}

impl<T> Lazy<T>
where
    T: Default
{
    pub fn get<P: platform::Platform>(&self) -> &T {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            match state {
                STATE_UNINIT => match self.state.compare_exchange_weak(
                    STATE_UNINIT,
                    STATE_LOADING,
                    Ordering::Acquire,
                    Ordering::Acquire,
                ) {
                    Err(new_state) => {
                        state = new_state;
                        spin_loop_hint();
                    },
                    Ok(_) => {
                        state = STATE_INIT;
                        self.value.set(MaybeUninit::new(T::default()));
                        self.state.store(STATE_INIT, Ordering::Release);
                    },
                },
                STATE_LOADING => {
                    P::yield_now();
                    state = self.state.load(Ordering::Acquire);
                },
                STATE_INIT => unsafe {
                    return &* (&* self.value.as_ptr()).as_ptr()
                },
                _ => unsafe {
                    unreachable_unchecked()
                },
            }
        }
    }
}
