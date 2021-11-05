use super::{
    context::Context,
    queue::{Queue, Runnable, Steal},
    random::RandomGenerator,
};
use std::{hint::spin_loop, mem::replace};

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
}

impl<'a> WorkerContext<'a> {
    pub fn new(context: &'a Context, worker_index: Option<usize>) -> Self {
        let mut rng = RandomGenerator::new();
        let tick = rng.gen();

        let mut worker_context = Self {
            context,
            tick,
            searching: false,
            rng,
            worker_index: None,
        };

        if let Some(worker_index) = worker_index {
            worker_context.transition_to_running(worker_index);
        }

        worker_context
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

    pub fn poll<F: Future>(&mut self, future: Pin<&mut F>) -> Result<Poll<F::Output>> {
        while let Some(runnable) = self.poll(future.as_mut()) {
            if replace(&mut self.searching, false) {
                self.context.executor.search_discovered();
            }

            self.tick = self.tick.wrapping_add(1);
            runnable.run(self.context);
        }
    }

    fn poll_runnable(&mut self) -> Result<Option<Runnable>, ()> {
        if let Some(worker_index) = self.worker_index {
            if let Some(runnable) = self.pop(worker_index) {
                return Ok(Some(runnable));
            }

            if self.transition_to_idle(worker_index) {
                if let Some(worker_index) = self.context.executor.search_retry() {
                    self.transition_to_running(worker_index);
                    continue;
                }
            }
        }

        match self.context.executor.thread_pool.wait() {
            Ok(worker_index) => self.transition_to_running(worker_index),
            Err(()) => return None,
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
}
