use super::{
    queue::{Queue as RunQueue, Runnable},
    executor::Executor,
    context::Context,
    parker::Parker,
    task::TaskError,
};
use std::{
    future::Future,
    pin::Pin,
    panic,
    sync::Arc,
    task::{Poll, Context as PollContext},
};

#[derive(Default)]
pub struct Worker {
    run_queue: RunQueue,
}

impl Worker {
    pub fn block_on<F: Future>(executor: &Arc<Executor>, mut worker_index: Option<usize>, future: Pin<&mut F>) -> F::Output {
        thread_local!(static TLS_PARKER: Arc<Parker> = Arc::new(Parker::default()));
        let parker = TLS_PARKER.with(|parker| parker.clone());

        let context = Context::enter(executor);
        let tick = context.rng.borrow_mut().next();

        let mut worker_thread = WorkerThread {
            tick,
            searching: false,
            context,
            parker,
            future: Some(future),
        };

        worker_index = worker_index.or_else(|| executor.search_retry());
        if let Some(worker_index) = worker_index {
            worker_thread.transition_to_notified(worker_index);
        }

        let result = worker_thread.block_on();

        worker_index = context.worker_index.get();
        if let Some(worker_index) = worker_index {
            worker_thread.transition_to_idle(worker_index);
        }

        match result {
            Some(output) => output,
            Err(error) => panic::resume_unwind(error),
        }
    }
}

struct WorkerThread<'a, F: Future> {
    tick: usize,
    searching: bool,
    context: Context,
    parker: Arc<Parker>,
    future: Option<Pin<&'a mut F>>,
}

impl<'a, F: Future> WorkerThread<'a, F> {
    fn transition_to_notified(&self, worker_index: usize) {
        assert!(self.context.worker_index.get().is_none());
        self.context.worker_index.set(Some(worker_index));

        let mut producer = self.context.producer.borrow_mut();
        assert!(producer.is_none());

        let worker = &self.context.executor.workers[worker_index];
        *producer = worker.run_queue.swap_producer(None);
        assert!(producer.is_some());
    }

    fn transition_to_idle(&self, worker_index: usize) {
        assert_eq!(self.context.worker_index.get(), Some(worker_index));
        self.context.worker_index.set(None);

        let mut producer = self.context.producer.take();
        assert!(producer.is_some());

        let worker = &self.context.executor.workers[worker_index];
        let old_producer = worker.run_queue.swap_producer(producer);
        assert!(old_producer.is_none());
    }

    fn block_on(&mut self) -> Result<F::Output, TaskError> {
        loop {
            if let Some(result) = self.poll_future() {
                return result;
            }
        }
    }

    fn poll_future(&mut self) -> Option<Poll<Result<F::Output, TaskError>>> {
        let future = self.future.take().unwrap();
        let parker = self.parker.clone();
        
        let result = self.parker.task_state.poll(parker, future.as_mut());

        self.future = Some(future);
        result
    }
}


