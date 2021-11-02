use super::{
    executor::Executor,
    pool::{Notified, WaitError},
    queue::{Producer, Runnable, Steal},
    random::RandomSource,
};
use crate::io::Poller as IoPoller;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    hint::spin_loop,
    mem,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

pub struct Thread {
    pub be_fair: Cell<bool>,
    pub executor: Arc<Executor>,
    pub worker_index: Cell<Option<usize>>,
    pub producer: RefCell<Option<Producer>>,
    pub io_intercept: RefCell<Option<VecDeque<Runnable>>>,
}

impl Thread {
    fn with_tls<F>(f: impl FnOnce(&mut Option<Rc<Self>>) -> F) -> F {
        thread_local!(static TLS: RefCell<Option<Rc<Thread>>> = RefCell::new(None));
        TLS.with(|ref_cell| f(&mut *ref_cell.borrow_mut()))
    }

    pub fn try_with<F>(f: impl FnOnce(&Thread) -> F) -> Option<F> {
        Self::with_tls(|tls| tls.as_ref().map(Rc::clone)).map(|rc| f(&*rc))
    }

    pub fn run(executor: &Arc<Executor>, notified: Notified) {
        let enter = ThreadEnter::from(executor);
        ThreadRef::run(enter.executor, &*enter.thread, notified);
    }
}

pub struct ThreadEnter<'a> {
    executor: &'a Arc<Executor>,
    thread: Rc<Thread>,
}

impl<'a> From<&'a Arc<Executor>> for ThreadEnter<'a> {
    fn from(executor: &'a Arc<Executor>) -> Self {
        let thread = Rc::new(Thread {
            be_fair: Cell::new(false),
            executor: executor.clone(),
            worker_index: Cell::new(None),
            producer: RefCell::new(None),
            io_intercept: RefCell::new(None),
        });

        match Thread::with_tls(|tls| mem::replace(tls, Some(thread.clone()))) {
            Some(_) => unreachable!("Nested runtimes are not supported"),
            None => {}
        }

        Self { executor, thread }
    }
}

impl<'a> Drop for ThreadEnter<'a> {
    fn drop(&mut self) {
        match Thread::with_tls(|tls| mem::replace(tls, None)) {
            Some(old_thread) => assert!(Rc::ptr_eq(&self.thread, &old_thread)),
            None => unreachable!("Thread was removed from thread_local storage"),
        }
    }
}

struct ThreadRef<'a, 'b> {
    thread: &'a Thread,
    executor: &'b Arc<Executor>,
    io_poller: IoPoller,
    io_ready: Option<VecDeque<Runnable>>,
    tick: usize,
    searching: bool,
    rng_source: RandomSource,
}

impl<'a, 'b> ThreadRef<'a, 'b> {
    fn run(executor: &'b Arc<Executor>, thread: &'a Thread, notified: Notified) {
        let mut rng_source = RandomSource::new(notified.worker_index);
        let tick = rng_source.next();

        let mut thread_ref = Self {
            thread,
            executor,
            io_poller: IoPoller::default(),
            io_ready: Some(VecDeque::new()),
            tick,
            searching: false,
            rng_source,
        };

        thread_ref.transition_to_notified(notified);
        thread_ref.run_thread();
    }

    fn transition_to_notified(&mut self, notified: Notified) {
        let producer = self.executor.workers[notified.worker_index]
            .run_queue
            .swap_producer(None)
            .expect("Thread running on Worker without a Producer");

        self.thread
            .producer
            .borrow_mut()
            .replace(producer)
            .map(|_| unreachable!("Thread notified when it already has a Producer"));

        assert_eq!(self.thread.worker_index.get(), None);
        self.thread.worker_index.set(Some(notified.worker_index));

        assert_eq!(self.searching, false);
        self.searching = notified.searching;
    }

    fn transition_to_idle(&mut self, worker_index: usize) -> bool {
        let producer = self
            .thread
            .producer
            .borrow_mut()
            .take()
            .expect("Thread becoming idle without a Producer");

        self.executor.workers[worker_index]
            .run_queue
            .swap_producer(Some(producer))
            .map(|_| unreachable!("Thread relenquishing Worker that already has a Producer"));

        assert_eq!(self.thread.worker_index.get(), Some(worker_index));
        self.thread.worker_index.set(None);

        let was_searching = mem::replace(&mut self.searching, false);
        self.executor.search_failed(worker_index, was_searching)
    }

    fn run_thread(&mut self) {
        while let Some(runnable) = self.poll() {
            if mem::replace(&mut self.searching, false) {
                self.executor.search_discovered();
            }

            self.tick = self.tick.wrapping_add(1);
            runnable.run(self.thread);
        }
    }

    fn poll(&mut self) -> Option<Runnable> {
        let executor = self.executor;
        loop {
            if let Some(worker_index) = self.thread.worker_index.get() {
                if let Some(runnable) = self.poll_search(worker_index) {
                    return Some(runnable);
                }

                if self.transition_to_idle(worker_index) {
                    if let Some(notified) = executor.search_retry() {
                        self.transition_to_notified(notified);
                        continue;
                    }
                }
            }

            if let Some(mut io_ready) = self.poll_io(None) {
                let mut runnable = None;
                if let Some(notified) = executor.search_retry() {
                    runnable = Some(io_ready.pop_front().unwrap());
                    self.transition_to_notified(notified);
                }

                executor.schedule_with(io_ready.into_iter(), Some(self.thread), false);
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
        let executor = self.executor;
        let producer = self.thread.producer.borrow();
        let producer = producer
            .as_ref()
            .expect("Thread polling for work without a Producer");

        let mut be_fair = self.thread.be_fair.get();
        if be_fair {
            self.thread.be_fair.set(false);
            self.tick = 0;
        } else {
            be_fair = self.tick % 64 == 0;
        }

        if be_fair {
            if let Some(runnable) = producer.consume(&self.executor.injector) {
                return Some(runnable);
            }
        }

        if let Some(runnable) = producer.pop(be_fair) {
            return Some(runnable);
        }

        if let Some(runnable) = producer.consume(&self.executor.injector) {
            return Some(runnable);
        }

        if let Some(mut io_ready) = self.poll_io(Some(Duration::ZERO)) {
            let runnable = io_ready.pop_front().unwrap();
            executor.schedule_with(io_ready.into_iter(), Some(self.thread), false);
            return Some(runnable);
        }

        self.searching = self.searching || executor.search_begin();
        if self.searching {
            for _attempt in 0..32 {
                let mut was_contended = false;
                for steal_index in self.rng_source.iter(executor.rng_iter_gen) {
                    if steal_index == worker_index {
                        continue;
                    }

                    match producer.steal(&executor.workers[steal_index].run_queue) {
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

        producer.consume(&self.executor.injector)
    }

    fn poll_io(&mut self, timeout: Option<Duration>) -> Option<VecDeque<Runnable>> {
        let io_ready = self
            .io_ready
            .take()
            .expect("Polling for I/O without a VecDeque");

        match mem::replace(&mut *self.thread.io_intercept.borrow_mut(), Some(io_ready)) {
            Some(_) => unreachable!("Polling for I/O with io_intercept is already set"),
            None => {}
        }

        self.io_poller.poll(&*self.executor.io_driver, timeout);

        let io_ready = match mem::replace(&mut *self.thread.io_intercept.borrow_mut(), None) {
            Some(io_intercept) => io_intercept,
            None => unreachable!("Polled I/O without an io_intercept queue"),
        };

        if io_ready.len() > 0 {
            return Some(io_ready);
        }

        assert!(self.io_ready.is_none());
        self.io_ready = Some(io_ready);
        None
    }
}
