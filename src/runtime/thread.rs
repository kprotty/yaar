use super::{
    executor::Executor,
    pool::Notified,
    queue::{PopError, Producer, Task},
    rand::RandomSource,
};
use crate::io::driver::Poller as IoPoller;
use std::hint::spin_loop as spin_loop_hint;
use std::{cell::RefCell, collections::VecDeque, mem, rc::Rc, sync::Arc};

pub struct Thread {
    pub(crate) executor: Arc<Executor>,
    pub(super) producer: RefCell<Option<Producer>>,
    pub(super) ready: RefCell<VecDeque<Task>>,
}

impl Thread {
    fn with_tls<F>(f: impl FnOnce(&mut Option<Rc<RefCell<Thread>>>) -> F) -> F {
        thread_local!(static TLS: RefCell<Option<Rc<RefCell<Thread>>>> = RefCell::new(None));
        TLS.with(|ref_cell| f(&mut *ref_cell.borrow_mut()))
    }

    pub fn with_current<F>(f: impl FnOnce(&Thread) -> F) -> Option<F> {
        Self::with_tls(|tls| tls.as_ref().map(Rc::clone)).map(|rc| f(&*(&*rc).borrow()))
    }

    pub fn run(executor: &Arc<Executor>, notified: Notified) {
        let producer = executor.workers[notified.worker_index]
            .run_queue
            .swap_producer(None)
            .expect("Thread running without a producer");

        let thread = Rc::new(RefCell::new(Self {
            executor: Arc::clone(executor),
            producer: RefCell::new(Some(producer)),
            ready: RefCell::new(VecDeque::new()),
        }));

        match Self::with_tls(|tls| mem::replace(tls, Some(thread.clone()))) {
            Some(_) => unreachable!("Cannot run multiple runtimes in the same thread"),
            None => {
                ThreadRef::new(&*thread.borrow(), executor, notified).run();
                Self::with_tls(|tls| *tls = None);
            }
        }
    }
}

struct ThreadRef<'a, 'b> {
    thread: &'a Thread,
    executor: &'b Arc<Executor>,
    worker_index: Option<usize>,
    io_poller: IoPoller,
    io_ready: VecDeque<Task>,
    prng: RandomSource,
    searching: bool,
    tick: usize,
}

impl<'a, 'b> ThreadRef<'a, 'b> {
    fn new(thread: &'a Thread, executor: &'b Arc<Executor>, notified: Notified) -> Self {
        let mut prng = RandomSource::default();
        let tick = prng.next();

        Self {
            thread,
            executor,
            worker_index: Some(notified.worker_index),
            io_poller: IoPoller::default(),
            io_ready: VecDeque::new(),
            prng,
            searching: notified.searching,
            tick,
        }
    }

    fn transition_to_running(&mut self, notified: Notified) {
        let producer = self.executor.workers[notified.worker_index]
            .run_queue
            .swap_producer(None)
            .expect("Thread notified without a producer");

        let mut thread_producer = self.thread.producer.borrow_mut();
        assert!(thread_producer.is_none());
        *thread_producer = Some(producer);

        self.worker_index = Some(notified.worker_index);
        self.searching = notified.searching;
    }

    fn transition_to_idle(&mut self, worker_index: usize) -> bool {
        let producer = self
            .thread
            .producer
            .borrow_mut()
            .take()
            .expect("Thread becoming idle without a producer");

        self.executor.workers[worker_index]
            .run_queue
            .swap_producer(Some(producer))
            .map(|_| unreachable!("Producer given back to worker that already has one"));

        self.worker_index = None;
        mem::replace(&mut self.searching, false)
    }

    fn run(&mut self) {
        while let Some(task) = self.next() {
            if self.searching {
                self.searching = false;
                self.executor.search_discovered();
            }

            self.tick = self.tick.wrapping_add(1);
            task.run(self.executor, self.thread)
        }
    }

    fn next(&mut self) -> Option<Task> {
        loop {
            while let Some(worker_index) = self.worker_index {
                if let Some(task) = self.poll(worker_index) {
                    return Some(task);
                }

                let was_searching = self.transition_to_idle(worker_index);
                let has_pending = self.executor.search_failed(worker_index, was_searching);

                if has_pending {
                    if let Some(worker_index) = self.executor.search_retry() {
                        self.transition_to_running(Notified {
                            worker_index,
                            searching: true,
                        });
                    }
                }
            }

            if self.io_poller.poll(&self.executor.io_driver, None) {
                assert_eq!(self.io_ready.len(), 0);
                mem::swap(&mut *self.thread.ready.borrow_mut(), &mut self.io_ready);

                if self.io_ready.len() > 0 {
                    if let Some(worker_index) = self.executor.search_retry() {
                        self.transition_to_running(Notified {
                            worker_index,
                            searching: true,
                        });
                    }

                    self.executor
                        .schedule(self.io_ready.drain(..), Some(self.thread));
                    continue;
                }
            }
            
            match self.executor.thread_pool.wait(None) {
                Ok(Some(notified)) => self.transition_to_running(notified),
                Ok(None) => {}
                Err(()) => return None,
            }
        }
    }

    fn poll(&mut self, worker_index: usize) -> Option<Task> {
        let executor = &self.executor;
        let run_queue = &executor.workers[worker_index].run_queue;

        let mut producer = self.thread.producer.borrow_mut();
        let producer = producer
            .as_mut()
            .expect("Thread polling without a producer");

        let be_fair = self.tick % 64 == 0;
        if let Some(task) = match be_fair {
            true => producer
                .consume(&executor.injector)
                .or_else(|| producer.pop(run_queue, be_fair)),
            _ => producer
                .pop(run_queue, be_fair)
                .or_else(|| producer.consume(&executor.injector)),
        } {
            return Some(task);
        }

        if !self.searching {
            self.searching = executor.search_begin();
        }

        if self.searching {
            for _ in 0..32 {
                let mut was_contended = false;
                for steal_index in self.prng.iter(executor.iter_gen) {
                    if steal_index == worker_index {
                        continue;
                    }

                    let target_queue = &executor.workers[steal_index].run_queue;
                    match producer.steal(target_queue) {
                        Ok(task) => return Some(task),
                        Err(PopError::Empty) => {}
                        Err(PopError::Contended) => was_contended = true,
                    }
                }

                if let Some(task) = producer.consume(&executor.injector) {
                    return Some(task);
                }

                if was_contended {
                    spin_loop_hint();
                    continue;
                }
            }
        }

        None
    }
}
