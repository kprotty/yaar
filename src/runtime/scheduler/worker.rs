use super::{
    context::Context,
    executor::Executor,
    parker::Parker,
    queue::{Queue as RunQueue, Runnable},
    task::JoinError,
};
use std::{
    future::Future,
    panic::{catch_unwind, resume_unwind, AssertUnwindSafe},
    pin::Pin,
    sync::Arc,
    task::{Context as PollContext, Poll},
};

#[derive(Default)]
pub struct Worker {
    run_queue: RunQueue,
}

impl Worker {
    pub fn block_on<F: Future>(
        executor: &Arc<Executor>,
        mut worker_index: Option<usize>,
        future: Pin<&mut F>,
    ) -> F::Output {
        thread_local!(static TLS_PARKER: Arc<Parker> = Arc::default());
        let parker = TLS_PARKER.with(|parker| parker.clone());

        let context = Context::enter(executor);
        let tick = context.rng.borrow_mut().next();

        let mut worker_thread = WorkerThread {
            tick: Cell::new(tick),
            searching: Cell::new(false),
            context,
            parker,
            future: RefCell::new(future),
        };

        worker_index = worker_index.or_else(|| executor.search_retry());
        if let Some(worker_index) = worker_index {
            worker_thread.transition_to_notified(worker_index);
        }

        *worker_thread.parker.context.try_lock().unwrap() = Some(worker_thread.context.clone());
        let result = worker_thread.block_on();
        *worker_thread.parker.context.try_lock().unwrap() = None;

        worker_index = context.worker_index.get();
        if let Some(worker_index) = worker_index {
            worker_thread.transition_to_idle(worker_index);
        }

        match result {
            Some(result) => result,
            Err(JoinError(None)) => unreachable!("cancelled"),
            Err(JoinError(Some(error))) => resume_unwind(error),
        }
    }
}

struct WorkerThread<'a, F: Future> {
    tick: Cell<usize>,
    searching: Cell<bool>,
    context: Context,
    parker: Arc<Parker>,
    future: RefCell<Pin<&'a mut F>>,
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

        assert!(!self.searching.replace(true));
    }

    fn transition_to_idle(&self, worker_index: usize) -> bool {
        assert_eq!(self.context.worker_index.get(), Some(worker_index));
        self.context.worker_index.set(None);

        let mut producer = self.context.producer.take();
        assert!(producer.is_some());

        let worker = &self.context.executor.workers[worker_index];
        let old_producer = worker.run_queue.swap_producer(producer);
        assert!(old_producer.is_none());

        self.searching.replace(false)
    }

    fn block_on(&self) -> Result<F::Output, JoinError> {
        loop {
            let retry_future = match self.poll_future() {
                None => false,
                Some(Poll::Pending) => true,
                Some(Poll::Ready(result)) => return result,
            };

            if let Some(runnable) = self.poll_runnable() {
                self.poll_task(|| runnable.run(&self.context));
                continue;
            }

            if retry_future {
                continue;
            }
        }
    }

    fn poll_task<T>(&self, poll_fn: impl FnOnce() -> T) -> T {
        if self.searching.replace(false) {
            self.context.executor.search_discovered();
        }

        let tick = self.tick.get();
        self.tick.set(tick.wrapping_add(1));

        // TODO: add task poll tracing
        poll_fn()
    }

    fn poll_future(&self) -> Option<Poll<Result<F::Output, JoinError>>> {
        let poll_result = self
            .parker
            .task_state
            .poll(&self.context, &self.parker, |ctx| {
                self.poll_task(|| {
                    let mut future = self.future.borrow_mut();
                    catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(ctx)))
                })
            });

        match poll_result {
            TaskJoin::Retry => Some(Poll::Pending),
            TaskJoin::Pending => None,
            TaskJoin::Ready(result) => Some(Poll::Ready(Ok(result))),
            TaskJoin::Shutdown => Some(Poll::Ready(Err(JoinError(None)))),
            TaskJoin::Panic(error) => Some(Poll::Ready(Err(JoinError(Some(error))))),
        }
    }

    fn poll_runnable(&self) -> Option<Runnable> {
        loop {
            let worker_index = self.context.worker_index.get()?;
            let producer = self.context.producer.borrow().unwrap();

            if self.tick.get() % 61 == 0 {
                if let Some(runnable) = self.poll_io() {
                    return Some(runnable);
                }

                if let Some(runnable) = producer.consume(&self.context.executor.injector).success()
                {
                    return Some(runnable);
                }
            }

            if let Some(runnable) = self.poll_timers() {
                return Some(runnable);
            }

            if let Some(runnable) = producer.pop() {
                return Some(runnable);
            }

            if !self.searching.get() {
                self.searching.set(self.context.executor.search_begin());
            }

            if self.searching.get() {
                if let Some(runnable) = self.poll_steal() {
                    return Some(runnable);
                }
            }

            let was_searching = self.transition_to_idle();
            match self
                .context
                .executor
                .search_failed(was_searching, worker_index)
            {
                Some(worker_index) => self.transition_to_notified(worker_index),
                None => return None,
            }
        }
    }

    fn poll_timers(&self) -> Option<Runnable> {}

    fn poll_io(&self) -> Option<Runnable> {}

    fn poll_steal(&self) -> Option<Runnable> {}
}
