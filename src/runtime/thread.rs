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
    pub(super) worker_index: Option<usize>,
    pub(super) producer: Option<Producer>,
    pub(super) ready: RefCell<VecDeque<Task>>,
    pub(super) is_polling: bool,
    is_searching: bool,
    prng: RandomSource,
    tick: usize,
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
            producer: executor.workers[notified.worker_index]
                .run_queue
                .swap_producer(None),
            ready: RefCell::new(VecDeque::new()),
            is_polling: false,
            is_searching: notified.searching,
            prng: RandomSource::default(),
            tick: 0,
        }));

        Self::with_thread(thread, |thread| loop {
            let task = match Self::poll(&*thread, executor) {
                Some(task) => task,
                None => return,
            };

            {
                let mut this = thread.borrow_mut();
                this.tick = this.tick.wrapping_add(1);
                if mem::replace(&mut this.is_searching, false) {
                    executor.search_discovered();
                }
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
                executor.workers[worker_index]
                    .run_queue
                    .swap_producer(this.producer.take());

                let was_searching = mem::replace(&mut this.is_searching, false);
                if executor.search_failed(worker_index, was_searching) {
                    if let Some(worker_index) = executor.search_retry() {
                        this.worker_index = Some(worker_index);
                        this.is_searching = true;
                        this.producer =
                            executor.workers[worker_index].run_queue.swap_producer(None);
                    }
                }
            }

            mem::drop(this);
            let waited = thread.borrow().wait(executor);
            let mut this = thread.borrow_mut();

            match waited {
                Ok(Some(notified)) => {
                    this.worker_index = Some(notified.worker_index);
                    this.is_searching = notified.searching;
                    this.producer = executor.workers[notified.worker_index]
                        .run_queue
                        .swap_producer(None);
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
        let run_queue = &executor.workers[worker_index].run_queue;
        let producer = self
            .producer
            .as_mut()
            .expect("Polling worker without a producer");

        let be_fair = self.tick % 64 == 0;
        if let Some(task) = match be_fair {
            true => producer.consume(&executor.injector).or_else(|| producer.pop(run_queue, true)),
            _ => producer.pop(run_queue, false).or_else(|| producer.consume(&executor.injector)),
        } {
            return Some(task);
        }

        mem::drop(producer);
        if !self.is_searching {
            self.is_searching = executor.search_begin();
        }

        let producer = self.producer.as_mut().unwrap();
        if self.is_searching {
            let mut attempts = 32;
            loop {
                let mut was_contended = false;
                for steal_index in self.prng.iter(executor.iter_gen) {
                    if steal_index == worker_index {
                        continue;
                    }

                    let target_queue = &executor.workers[steal_index].run_queue;
                    match producer.steal(target_queue) {
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

        producer.consume(&executor.injector)
    }

    fn wait(&self, executor: &Arc<Executor>) -> Result<Option<Notified>, ()> {
        executor.thread_pool.wait(None)
    }
}
