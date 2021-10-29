use super::{
    executor::Executor,
    pool::Notified,
    queue::{PopError, Producer, Task},
    rand::RandomSource,
};
use std::hint::spin_loop as spin_loop_hint;
use std::{cell::RefCell, collections::VecDeque, mem, rc::Rc, sync::Arc};

pub struct Thread {
    pub(super) executor: Arc<Executor>,
    pub(super) producer: Option<Producer>,
    pub(super) poll_ready: VecDeque<Task>,
    pub(super) is_polling: bool,
    worker_index: Option<usize>,
    is_searching: bool,
    prng: RandomSource,
}

impl Thread {
    fn with_tls<F>(f: impl FnOnce(&mut Option<Rc<RefCell<Thread>>>) -> F) -> F {
        thread_local!(static TLS: RefCell<Option<Rc<RefCell<Thread>>>> = RefCell::new(None));
        TLS.with(|ref_cell| f(&mut *ref_cell.borrow_mut()))
    }

    pub fn with_current<F>(f: impl FnOnce(&mut Thread) -> F) -> Option<F> {
        Self::with_tls(|tls| tls.as_ref().map(Rc::clone)).map(|rc| f(&mut *(&*rc).borrow_mut()))
    }

    pub fn run(executor: &Arc<Executor>, notified: Notified) {
        let thread = Rc::new(RefCell::new(Self {
            executor: Arc::clone(executor),
            worker_index: Some(notified.worker_index),
            producer: executor.workers[notified.worker_index].run_queue.swap_producer(None),
            poll_ready: VecDeque::new(),
            is_polling: false,
            is_searching: notified.searching,
            prng: RandomSource::default(),
        }));

        match Self::with_tls(|tls| mem::replace(tls, Some(thread.clone()))) {
            Some(_) => unreachable!("Cannot run multiple runtimes in the same thread"),
            None => {}
        }

        while let Some(task) = Self::poll(&*thread, executor) {
            let was_searching = {
                let mut thread = (&*thread).borrow_mut();
                mem::replace(&mut thread.is_searching, false)
            };
            if was_searching {
                executor.search_discovered();
            }   
            task.run(executor, &*thread);
        }

        Self::with_tls(|tls| *tls = None);
    }

    fn poll(thread: &RefCell<Thread>, executor: &Arc<Executor>) -> Option<Task> {
        loop {
            let mut this = thread.borrow_mut();
            if let Some(worker_index) = this.worker_index {
                if let Some(task) = this.producer.as_ref().unwrap().pop() {
                    return Some(task);
                }

                if let Ok(task) = this.producer.as_ref().unwrap().consume(&executor.injector) {
                    return Some(task);
                }

                if !this.is_searching {
                    this.is_searching = executor.search_begin();
                }

                if this.is_searching {
                    let mut attempts = 32usize;
                    loop {
                        let mut was_contended = false;
                        for steal_index in this.prng.iter(executor.iter_gen) {
                            if steal_index == worker_index {
                                continue;
                            }

                            match this.producer.as_ref().unwrap().steal(&executor.workers[steal_index].run_queue) {
                                Ok(task) => return Some(task),
                                Err(PopError::Empty) => {},
                                Err(PopError::Contended) => was_contended = true,
                            }
                        }
                        
                        spin_loop_hint();
                        if was_contended && attempts != 0 {
                            attempts -= 1;
                        } else {
                            break;
                        }
                    }
                }

                if let Ok(task) = this.producer.as_ref().unwrap().consume(&executor.injector) {
                    return Some(task);
                }
                
                let p = executor
                    .workers[worker_index]
                    .run_queue
                    .swap_producer(this.producer.take());
                assert!(p.is_none());

                this.worker_index = None;
                let was_searching = mem::replace(&mut this.is_searching, false);

                if executor.search_failed(worker_index, was_searching) {
                    this.worker_index = executor.search_retry();
                    this.is_searching = this.worker_index.is_some();
                    continue;
                }
            }

            // TODO: poll timers & IO
            mem::drop(this);

            match executor.thread_pool.wait(None) {
                Ok(Some(notified)) => {
                    let mut this = thread.borrow_mut();
                    this.producer = executor.workers[notified.worker_index].run_queue.swap_producer(None);
                    this.worker_index = Some(notified.worker_index);
                    this.is_searching = notified.searching;
                }
                Ok(None) => continue,
                Err(()) => return None,
            };
        }
    }

    pub fn pending(executor: &Executor) -> bool {
        executor.injector.pending() || executor
            .workers
            .iter()
            .map(|worker| worker.run_queue.pending())
            .filter(|pending| !pending)
            .next()
            .unwrap_or(false)
    }
}
