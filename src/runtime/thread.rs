use super::{
    executor::Executor,
    pool::Notified,
    queue::{PopError, Task},
    rand::RandomSource,
};
use std::hint::spin_loop as spin_loop_hint;
use std::{cell::RefCell, collections::VecDeque, mem, rc::Rc, sync::Arc};

pub struct Thread {
    pub(super) executor: Arc<Executor>,
    pub(super) worker_index: Option<usize>,
    pub(super) ready: RefCell<VecDeque<Task>>,
    pub(super) is_polling: bool,
    is_searching: bool,
    prng: RandomSource,
}

impl Thread {
    fn with_tls<F>(f: impl FnOnce(&mut Option<Rc<RefCell<Thread>>>) -> F) -> F {
        thread_local!(static TLS: RefCell<Option<Rc<RefCell<Thread>>>> = RefCell::new(None));
        TLS.with(|ref_cell| f(&mut *ref_cell.borrow_mut()))
    }

    pub fn with_current<F>(f: impl FnOnce(&Thread) -> F) -> Option<F> {
        Self::with_tls(|tls| tls.as_ref().map(Rc::clone)).map(|rc| f(&*(&*rc).borrow()))
    }

    fn with_thread(thread: Rc<RefCell<Self>>, f: impl FnOnce(&RefCell<Self>)) {
        match Self::with_tls(|tls| mem::replace(tls, Some(thread.clone()))) {
            Some(_) => unreachable!("Cannot run multiple runtimes in the same thread"),
            None => {
                f(&*thread);
                Self::with_tls(|tls| *tls = None);
            }
        }
    }

    pub fn run(executor: &Arc<Executor>, notified: Notified) {
        let thread = Rc::new(RefCell::new(Self {
            executor: Arc::clone(executor),
            worker_index: Some(notified.worker_index),
            ready: RefCell::new(VecDeque::new()),
            is_polling: false,
            is_searching: notified.searching,
            prng: RandomSource::default(),
        }));

        Self::with_thread(thread, |thread| loop {
            let task = match Self::poll(&*thread, executor) {
                Some(task) => task,
                None => return,
            };

            if mem::replace(&mut thread.borrow_mut().is_searching, false) {
                executor.search_discovered();
            }

            task.run(executor, &*thread.borrow());
        })
    }

    fn poll<'a>(thread: &'a RefCell<Self>, executor: &Arc<Executor>) -> Option<Task> {
        loop {
            let mut this = thread.borrow_mut();
            while let Some(worker_index) = this.worker_index {
                if let Some(task) = this.pop(worker_index, executor) {
                    return Some(task);
                }

                this.worker_index = None;
                let was_searching = mem::replace(&mut this.is_searching, false);

                if executor.search_failed(worker_index, was_searching) {
                    this.worker_index = executor.search_retry();
                    this.is_searching = this.worker_index.is_some();
                }
            }

            mem::drop(this);
            let waited = thread.borrow().wait(executor);
            let mut this = thread.borrow_mut();

            match waited {
                Ok(Some(notified)) => {
                    this.worker_index = Some(notified.worker_index);
                    this.is_searching = notified.searching;
                }
                Ok(None) => {}
                Err(()) => return None,
            }

            let mut ready = mem::replace(&mut *this.ready.borrow_mut(), VecDeque::new());
            let mut ready = ready.drain(..);
            let task = this.worker_index.and_then(|_| ready.next());
            mem::drop(this);

            if ready.len() > 0 {
                executor.schedule(ready, Some(&*thread.borrow()));
            }

            if let Some(task) = task {
                return Some(task);
            }
        }
    }

    fn pop(&mut self, worker_index: usize, executor: &Arc<Executor>) -> Option<Task> {
        if let Some(task) = executor.workers[worker_index].run_queue.pop() {
            return Some(task);
        }

        if !self.is_searching {
            self.is_searching = executor.search_begin();
        }

        if self.is_searching {
            let mut attempts = 32;
            loop {
                let mut was_contended = false;
                for steal_index in self.prng.iter(executor.iter_gen) {
                    let run_queue = &executor.workers[worker_index].run_queue;
                    let target_queue = &executor.workers[steal_index].run_queue;

                    match run_queue.steal(target_queue) {
                        Ok(task) => return Some(task),
                        Err(PopError::Empty) => continue,
                        Err(PopError::Contended) => was_contended = true,
                    }
                }

                if was_contended && attempts != 0 {
                    attempts -= 1;
                    spin_loop_hint();
                } else {
                    break;
                }
            }
        }

        None
    }

    fn wait(&self, executor: &Arc<Executor>) -> Result<Option<Notified>, ()> {
        executor.thread_pool.wait(None)
    }
}
