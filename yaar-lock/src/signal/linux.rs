use core::{
    time::Duration,
    sync::atomic::{fence, Ordering, AtomicU32},
};

const EMPTY: u32 = 0;
const NOTIFY: u32 = 1;
const WAITER: u32 = 2;

pub struct SystemSignal {
    state: AtomicU32,
}

impl SystemSignal {
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(EMPTY),
        }
    }

    #[inline]
    pub fn notify(&self) {
        
    }

    pub fn notify_all(&self) {
        if self.state.load(Ordering::Relaxed) == EMPTY {
            if self.state.compare_exchange(EMPTY, NOTIFY, Ordering::Release, Ordering::Relaxed).is_ok() {
                return;
            }
        }
        
        self.state.swap(EMPTY, Ordering::Release);

    }

    #[inline]
    pub fn wait(&self) {
        
    }

    

    pub fn wait_timeout(&self, duration: Duration) -> bool {

    }
}