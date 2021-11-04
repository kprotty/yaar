use super::{
    queue::{Queue, Runnable, Steal},
    random::RandomGenerator,
    thread::Thread,
};
use std::{hint::spin_loop, mem::replace};

#[derive(Default)]
pub struct Worker {
    pub run_queue: Queue,
}

pub struct WorkerThread<'a> {
    thread: &'a Thread,
    tick: usize,
    searching: bool,
    rng: RandomGenerator,
    worker_index: Option<usize>,
}

impl<'a> WorkerThread<'a> {
    pub fn run(thread: &'a Thread, worker_index: usize, position: usize) {
        let mut rng = RandomGenerator::from(position);
        let tick = rng.gen();

        let mut thread_ref = Self {
            thread,
            tick,
            searching: false,
            rng,
            worker_index: None,
        };

        thread_ref.transition_to_running(worker_index);
        thread_ref.run_until_shutdown();
    }

    fn transition_to_running(&mut self, worker_index: usize) {
        assert!(self.worker_index.is_none());
        self.worker_index = Some(worker_index);

        assert!(!self.searching);
        self.searching = true;

        let producer = self.thread.executor.workers[worker_index]
            .run_queue
            .swap_producer(None)
            .unwrap();

        self.thread
            .producer
            .replace(Some(producer))
            .map(|_| unreachable!());
    }

    fn transition_to_idle(&mut self, worker_index: usize) -> bool {
        let producer = self.thread.producer.take().unwrap();
        self.thread.executor.workers[worker_index]
            .run_queue
            .swap_producer(Some(producer))
            .map(|_| unreachable!());

        assert_eq!(self.worker_index, Some(worker_index));
        self.worker_index = None;

        let was_searching = replace(&mut self.searching, false);
        self.thread
            .executor
            .search_failed(worker_index, was_searching)
    }

    fn run_until_shutdown(&mut self) {
        while let Some(runnable) = self.poll() {
            if replace(&mut self.searching, false) {
                self.thread.executor.search_discovered();
            }

            self.tick = self.tick.wrapping_add(1);
            runnable.run(self.thread);
        }
    }

    fn poll(&mut self) -> Option<Runnable> {
        loop {
            if let Some(worker_index) = self.worker_index {
                if let Some(runnable) = self.pop(worker_index) {
                    return Some(runnable);
                }

                if self.transition_to_idle(worker_index) {
                    if let Some(worker_index) = self.thread.executor.search_retry() {
                        self.transition_to_running(worker_index);
                        continue;
                    }
                }
            }

            match self.thread.executor.thread_pool.wait() {
                Ok(worker_index) => self.transition_to_running(worker_index),
                Err(()) => return None,
            }
        }
    }

    fn pop(&mut self, worker_index: usize) -> Option<Runnable> {
        let producer = self.thread.producer.borrow();
        let producer = producer.as_ref().unwrap();
        let executor = &self.thread.executor;

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