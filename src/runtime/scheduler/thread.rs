use super::{
    executor::Executor,
    pool::{Notified, WaitError},
    queue::{Runnable, Steal},
    random::RandomIterGen,
};
use crate::io::driver::Poller as IoPoller;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    hint::spin_loop,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

pub struct Thread {
    pub executor: Arc<Executor>,
    pub worker_index: Cell<Option<usize>>,
    pub use_ready: Cell<bool>,
    pub ready: RefCell<VecDeque<Runnable>>,
}

impl Thread {
    fn with_tls<F>(f: impl FnOnce(&mut Option<Rc<Self>>) -> F) -> F {
        thread_local!(static TLS: RefCell<Option<Rc<Thread>>> = RefCell::new(None));
        TLS.with(|ref_cell| f(&mut *ref_cell.borrow_mut()))
    }

    pub fn try_with<F>(f: impl FnOnce(&Thread) -> F) -> Option<F> {
        Self::with_tls(|tls| tls.as_ref().map(Rc::clone)).map(|rc| f(&*rc))
    }

    pub fn run(executor: &Arc<Executor>, notified: Notified) {}
}

struct ThreadRef<'a, 'b> {
    thread: &'a Thread,
    executor: &'b Arc<Executor>,
    io_poller: IoPoller,
    io_ready: VecDeque<Runnable>,
    tick: u8,
    searching: bool,
    rng_gen: RandomIterGen,
}

impl<'a, 'b> ThreadRef<'a, 'b> {
    fn transition_to_notified(&mut self, notified: Notified) {
        assert_eq!(self.thread.worker_index.get(), None);
        self.thread.worker_index.set(Some(notified.worker_index));

        assert_eq!(self.searching, false);
        self.searching = notified.searching;
    }

    fn poll(&mut self) -> Option<Task> {
        let thread = self.thread;
        let executor = self.executor;
        loop {
            if let Some(worker_index) = thread.worker_index.get() {
                if let Some(runnable) = self.poll_search(worker_index) {
                    return Some(runnable);
                }

                let was_searching = mem::replace(&mut self.searching, false);
                if executor.search_failed(worker_index, was_searching) {
                    if let Some(notified) = executor.search_retry() {
                        self.transition_to_notified(notified);
                        continue;
                    }
                }
            }

            if self.poll_for_io(None) {
                let mut runnable = None;
                if let Some(notified) = executor.search_retry() {
                    runnable = self.io_ready.pop_first();
                    self.transition_to_notified(notified);
                }

                executor.schedule(self.io_ready.drain(..), Some(thread));
                if runnable.is_some() {
                    return runnable;
                }
            }

            match executor.thread_pool.wait(None) {
                Ok(notified) => self.transition_to_notified(notified),
                Err(WaitError::Shutdown) => return None,
                Err(WaitError::TimedOut) => continue,
            }
        }
    }

    fn poll_search(&mut self, worker_index: usize) -> Option<Runnable> {
        let thread = self.thread;
        let executor = self.executor;
        let run_queue = &executor.workers[worker_index].run_queue;

        let be_fair = self.tick % 64 == 0;
        if let Some(runnable) = run_queue.pop(be_fair) {
            return Some(runnable);
        }

        if self.poll_io(Some(Duration::ZERO)) {
            let runnable = self.io_ready.pop_first().unwrap();
            executor.schedule(self.io_ready.drain(..), Some(thread));
            return Some(runnable);
        }

        self.searching = self.searching || executor.search_begin();
        if self.searching {
            for _attempt in 0..32 {
                let mut was_contended = match run_queue.consume(&executor.injector) {
                    Steal::Success(runnable) => return Some(runnable),
                    Steal::Empty => false,
                    Steal::Retry => true,
                };

                for steal_index in self.rng_gen.iter() {
                    if steal_index == worker_index {
                        continue;
                    }

                    let target_queue = &executor.workers[steal_index].run_queue;
                    match run_queue.steal(target_queue) {
                        Steal::Success(runnable) => return Some(runnable),
                        Steal::Retry => was_contended = true,
                        Steal::Empty => {}
                    }
                }

                match was_contended {
                    true => spin_loop(),
                    false => break,
                }
            }
        }

        None
    }

    fn poll_io(&mut self, timeout: Option<Duration>) -> bool {
        assert!(!self.thread.use_ready.replace(true));
        let polled = self.io_poller.poll(&*self.executor.io_driver, timeout);
        assert!(self.thread.use_ready.replace(false));

        polled && {
            assert_eq!(self.io_ready.len(), 0);
            mem::swap(&mut self.io_ready, &mut *self.thread.ready.borrow_mut());
            self.io_ready.len() > 0
        }
    }
}
