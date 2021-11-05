use super::{
    context::Context,
    queue::{Queue, Runnable, Steal},
    random::RandomGenerator,
};
use crate::io::driver::PollEvents;
use std::{collections::VecDeque, hint::spin_loop, mem::replace, time::Duration};

#[derive(Default)]
pub struct Worker {
    pub run_queue: Queue,
}

pub struct WorkerContext<'a> {
    context: &'a Context,
    tick: usize,
    searching: bool,
    rng: RandomGenerator,
    worker_index: Option<usize>,
    poll_events: PollEvents,
    poll_ready: VecDeque<Runnable>,
}

impl<'a> WorkerContext<'a> {
    pub fn run(context: &'a Context, worker_index: usize) {
        let mut rng = RandomGenerator::new();
        let tick = rng.gen();

        let mut worker_context = Self {
            context,
            tick,
            searching: false,
            rng,
            worker_index: None,
            poll_events: PollEvents::new(),
            poll_ready: VecDeque::new(),
        };

        worker_context.transition_to_running(worker_index);
        worker_context.run_until_shutdown();
    }

    fn transition_to_running(&mut self, worker_index: usize) {
        assert!(self.worker_index.is_none());
        self.worker_index = Some(worker_index);

        assert!(!self.searching);
        self.searching = true;

        let producer = self.context.executor.workers[worker_index]
            .run_queue
            .swap_producer(None)
            .unwrap();

        self.context
            .producer
            .replace(Some(producer))
            .map(|_| unreachable!());
    }

    fn transition_to_idle(&mut self, worker_index: usize) -> bool {
        let producer = self.context.producer.take().unwrap();
        self.context.executor.workers[worker_index]
            .run_queue
            .swap_producer(Some(producer))
            .map(|_| unreachable!());

        assert_eq!(self.worker_index, Some(worker_index));
        self.worker_index = None;

        let was_searching = replace(&mut self.searching, false);
        self.context
            .executor
            .search_failed(worker_index, was_searching)
    }

    fn run_until_shutdown(&mut self) {
        while let Some(runnable) = self.poll() {
            if replace(&mut self.searching, false) {
                self.context.executor.search_discovered();
            }

            self.tick = self.tick.wrapping_add(1);
            runnable.run(self.context);
        }
    }

    fn poll(&mut self) -> Option<Runnable> {
        loop {
            if let Some(worker_index) = self.worker_index {
                if let Some(runnable) = self.pop(worker_index) {
                    return Some(runnable);
                }

                if self.transition_to_idle(worker_index) {
                    if let Some(worker_index) = self.context.executor.search_retry() {
                        self.transition_to_running(worker_index);
                        continue;
                    }
                }
            }

            if let Some(runnable) = self.poll_io(None) {
                return Some(runnable);
            }

            match self.context.executor.thread_pool.wait() {
                Ok(worker_index) => self.transition_to_running(worker_index),
                Err(()) => return None,
            }
        }
    }

    fn pop(&mut self, worker_index: usize) -> Option<Runnable> {
        let producer = self.context.producer.borrow();
        let producer = producer.as_ref().unwrap();
        let executor = &self.context.executor;

        let be_fair = self.tick % 61 == 0;
        if be_fair {
            if let Some(runnable) = producer.consume(&executor.injector).success() {
                return Some(runnable);
            }
        }

        if let Some(runnable) = producer.pop(be_fair) {
            return Some(runnable);
        }

        if let Some(runnable) = self.poll_io(Some(Duration::ZERO)) {
            return Some(runnable);
        }

        self.searching = self.searching || executor.search_begin();
        if self.searching {
            for _retries in 0..32 {
                let mut was_contended = match producer.consume(&executor.injector) {
                    Steal::Success(runnable) => return Some(runnable),
                    Steal::Empty => false,
                    Steal::Retry => true,
                };

                for steal_index in self.rng.gen_iter(executor.rng_iter_source) {
                    if steal_index == worker_index {
                        continue;
                    }

                    match producer.steal(&executor.workers[steal_index].run_queue) {
                        Steal::Success(runnable) => return Some(runnable),
                        Steal::Retry => was_contended = true,
                        Steal::Empty => {}
                    }
                }

                if was_contended {
                    spin_loop();
                } else {
                    break;
                }
            }
        }

        None
    }

    fn poll_io(&mut self, timeout: Option<Duration>) -> Option<Runnable> {
        let executor = &self.context.executor;
        let mut poll_guard = match executor.io_driver.try_poll() {
            Some(guard) => guard,
            None => return None,
        };

        poll_guard.poll(&mut self.poll_events, timeout);
        drop(poll_guard);

        let io_ready = replace(&mut self.poll_ready, VecDeque::new());
        *self.context.intercept.borrow_mut() = Some(io_ready);

        self.poll_events.process(&executor.io_driver);

        let io_ready = self.context.intercept.borrow_mut().take().unwrap();
        drop(replace(&mut self.poll_ready, io_ready));

        let mut runnable = None;
        if self.poll_ready.len() > 0 {
            if self.worker_index.is_none() {
                if let Some(worker_index) = executor.search_retry() {
                    self.transition_to_running(worker_index);
                }
            }

            if self.worker_index.is_some() {
                runnable = self.poll_ready.pop_front();
            }

            if self.poll_ready.len() > 0 {
                let runnables = self.poll_ready.drain(..);
                executor.inject(runnables);
            }
        }

        runnable
    }
}
