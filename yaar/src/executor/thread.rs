use super::{Platform, Worker};
use core::{cell::Cell, ptr::NonNull};

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ThreadState {
    Idle,
    Polling,
    Searching,
    Running,
}

pub struct Thread<P: Platform> {
    pub data: P::ThreadLocalData,
    pub(crate) state: Cell<ThreadState>,
    pub(crate) next: Cell<Option<NonNull<Self>>>,
    pub(crate) worker: Cell<Option<NonNull<Worker<P>>>>,
    pub(crate) event: P::ThreadEvent,
}

impl<P: Platform> Thread<P> {
    pub fn state(&self) -> ThreadState {
        self.state.get();
    }

    pub fn worker(&self) -> Option<&Worker<P>> {
        self.worker.get().map(|ptr| unsafe { &*ptr.as_ptr() })
    }

    pub extern "C" fn run(worker: &Worker<P>) {
        let node = worker.node().expect("Thread running without a Node");

        let this = Self {
            data: P::ThreadLocalData::default(),
            state: Cell::new(match node.workers_searching.load(Ordering::Relaxed) {
                0 => ThreadState::Idle,
                _ => ThreadState::Searching,
            }),
            next: Cell::new(None),
            worker: Cell::new(NonNull::new(worker as *const _ as *mut _)),
            event: P::ThreadEvent::default(),
        };

        let this_ptr = NonNull::new(&this as *const _ as *mut _);
        worker.thread.set(this_ptr);
        P::set_tls_thread(this_ptr);

        let mut tick = 0;
        while let Some(task) = self.poll(node, tick) {
            let old_state = self.state.replace(ThreadState::Running);
            if old_state == ThreadState::Searching {
                if node.workers_searching.fetch_sub(1, Ordering::Release) == 1 {
                    node.spawn_worker();
                }
            }

            unsafe { task.as_ref().resume() };
            tick += 1;
        }
    }
}
