use super::{executor::Executor, pool::Notified, queue::Task};
use std::{cell::RefCell, collections::VecDeque, mem, rc::Rc, sync::Arc};

pub struct Thread {
    pub(super) executor: Arc<Executor>,
    pub(super) worker_index: Option<usize>,
    pub(super) poll_ready: RefCell<VecDeque<Task>>,
    pub(super) is_polling: bool,
    is_searching: bool,
}

impl Thread {
    fn with_tls<F>(f: impl FnOnce(&mut Option<Rc<RefCell<Thread>>>) -> F) -> F {
        thread_local!(static TLS: RefCell<Option<Rc<RefCell<Thread>>>> = RefCell::new(None));
        TLS.with(|ref_cell| f(&mut *ref_cell.borrow_mut()))
    }

    pub fn with_current<F>(f: impl FnOnce(&Thread) -> F) -> Option<F> {
        Self::with_tls(|tls| tls.as_ref().map(Rc::clone)).map(|rc| f(&*rc.borrow()))
    }

    pub fn run(executor: Arc<Executor>, notified: Notified) {
        let thread = Rc::new(RefCell::new(Self {
            executor,
            worker_index: Some(notified.worker_index),
            poll_ready: RefCell::new(VecDeque::new()),
            is_polling: false,
            is_searching: notified.searching,
        }));

        Self::with_tls(|tls| mem::replace(tls, Some(thread.clone())))
            .expect("Cannot run multiple runtimes in the same thread");

        while let Some(task) = Self::poll(&*thread) {
            let thread = thread.borrow();
            let worker_index = thread.worker_index.unwrap();
            task.run(&thread.executor, worker_index);
        }

        Self::with_tls(|tls| *tls = None);
    }

    fn poll(thread: &RefCell<Thread>) -> Option<Task> {
        loop {
            let mut this = thread.borrow_mut();
            while let Some(worker_index) = this.worker_index {
                if let Some(task) = this.pop(worker_index) {
                    return Some(task);
                }

                if !this.is_searching {
                    this.is_searching = this.executor.search_begin();
                }

                if this.is_searching {
                    if let Some(task) = this.search(worker_index) {
                        return Some(task);
                    }
                }

                this.worker_index = None;
                let was_searching = mem::replace(&mut this.is_searching, false);

                if this.executor.search_failed(worker_index, was_searching) {
                    this.worker_index = this.executor.search_retry();
                    this.is_searching = this.worker_index.is_some();
                }
            }

            // TODO: poll timers & IO
        }
    }

    fn pop(&mut self, worker_index: usize) -> Option<Task> {
        self.executor.workers[worker_index].run_queue.pop().ok()
    }

    fn search(&mut self, worker_index: usize) -> Option<Task> {
        unimplemented!("TODO")
    }
}
