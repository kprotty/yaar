use super::{
    context::Context,
    executor::Executor,
    parker::Parker,
    poller::Poller,
    queue::{Queue as RunQueue, Runnable, Steal},
    task::TaskError,
};
use crate::time::internal::queue::Queue as TimerQueue;
use pin_utils::pin_mut;
use std::{
    cell::RefCell,
    future::Future,
    hint::spin_loop,
    mem::replace,
    panic::{catch_unwind, resume_unwind, AssertUnwindSafe},
    pin::Pin,
    sync::Arc,
    task::{Context as PollContext, Poll, Wake, Waker},
    time::{Duration, Instant},
};

impl Wake for Parker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if self.task_state.transition_to_scheduled() {
            let _ = self.unpark(None);
        }
    }
}

pub struct Worker {
    pub run_queue: RunQueue,
    pub timer_queue: Arc<TimerQueue>,
}

pub struct WorkerContext {
    tick: usize,
    searching: bool,
    parker: Arc<Parker>,
    poller: RefCell<Poller>,
}

impl WorkerContext {
    pub fn block_on<F: Future>(
        executor: &Arc<Executor>,
        mut worker_index: Option<usize>,
        future: F,
    ) -> F::Output {
        pin_mut!(future);
        let future: Pin<&mut F> = future;

        let context_ref = Context::enter(executor);
        let context = context_ref.as_ref();

        let mut worker_context = Self {
            tick: context.rng.borrow_mut().gen(),
            searching: false,
            parker: Arc::new(Parker::new(executor.clone())),
            poller: RefCell::new(Poller::default()),
        };

        worker_index = worker_index.or_else(|| executor.search_retry());
        if let Some(worker_index) = worker_index {
            worker_context.transition_to_running(context, worker_index);
        }

        executor.thread_pool.task_begin();
        let result = worker_context.run(context, future);
        executor.thread_pool.task_complete();

        if let Some(worker_index) = context.worker_index.get() {
            if worker_context.transition_to_idle(context, worker_index) {
                executor.notify();
            }
        }

        match result {
            Ok(result) => result,
            Err(error) => resume_unwind(error),
        }
    }

    pub fn run<F: Future>(
        &mut self,
        context: &Context,
        mut future: Pin<&mut F>,
    ) -> Result<F::Output, TaskError> {
        let scheduled_future = self.parker.task_state.transition_to_scheduled();
        assert!(scheduled_future);

        loop {
            let poll_result = self.poll(context, future.as_mut());

            self.tick = self.tick.wrapping_add(1);
            if replace(&mut self.searching, false) {
                context.executor.search_discovered();
            }

            match poll_result {
                Ok(result) => return result,
                Err(runnable) => runnable.run(context),
            }
        }
    }

    fn transition_to_running(&mut self, context: &Context, worker_index: usize) {
        assert!(context.worker_index.get().is_none());
        context.worker_index.set(Some(worker_index));

        assert!(!self.searching);
        self.searching = true;

        let producer = context.executor.workers[worker_index]
            .run_queue
            .swap_producer(None)
            .unwrap();

        context
            .producer
            .replace(Some(producer))
            .map(|_| unreachable!());
    }

    fn transition_to_idle(&mut self, context: &Context, worker_index: usize) -> bool {
        let producer = context.producer.take().unwrap();
        context.executor.workers[worker_index]
            .run_queue
            .swap_producer(Some(producer))
            .map(|_| unreachable!());

        assert_eq!(context.worker_index.get(), Some(worker_index));
        context.worker_index.set(None);

        let was_searching = replace(&mut self.searching, false);
        context.executor.search_failed(worker_index, was_searching)
    }

    fn poll<F: Future>(
        &mut self,
        context: &Context,
        mut future: Pin<&mut F>,
    ) -> Result<Result<F::Output, TaskError>, Runnable> {
        loop {
            let mut now = None;
            let mut timeout = None;

            if let Some(result) = self.poll_future(future.as_mut()) {
                return Ok(result);
            }

            if let Some(worker_index) = context.worker_index.get() {
                if let Some(runnable) =
                    self.poll_worker(context, worker_index, &mut now, &mut timeout)
                {
                    return Err(runnable);
                }

                if self.transition_to_idle(context, worker_index) {
                    if let Some(worker_index) = context.executor.search_retry() {
                        self.transition_to_running(context, worker_index);
                        continue;
                    }
                }
            }

            let mut poller = self.poller.borrow_mut();
            let mut worker_index =
                context
                    .executor
                    .thread_pool
                    .wait(&self.parker, &mut *poller, context, timeout);

            let notified = poller.pending() || timeout.is_some();
            if notified && worker_index.is_none() {
                worker_index = context.executor.search_retry();
            }

            if let Some(worker_index) = worker_index {
                let runnable = poller.pop_and_inject(context);
                drop(poller);

                self.transition_to_running(context, worker_index);
                match runnable {
                    Some(runnable) => return Err(runnable),
                    None => continue,
                }
            }

            poller.inject(context);
            if !notified {
                let _ = self.parker.task_state.transition_to_scheduled();
            }
        }
    }

    fn poll_future<F: Future>(
        &mut self,
        future: Pin<&mut F>,
    ) -> Option<Result<F::Output, TaskError>> {
        if !self.parker.task_state.transition_to_running() {
            return None;
        }

        let poll_result = catch_unwind(AssertUnwindSafe(|| {
            let waker = Waker::from(self.parker.clone());
            let mut ctx = PollContext::from_waker(&waker);
            future.poll(&mut ctx)
        }));

        let result = match poll_result {
            Err(error) => Err(error),
            Ok(Poll::Ready(result)) => Ok(result),
            Ok(Poll::Pending) => {
                if !self.parker.task_state.transition_to_idle() {
                    self.parker
                        .task_state
                        .transition_to_scheduled_from_notified();
                }

                return None;
            }
        };

        Some(result)
    }

    fn poll_worker(
        &mut self,
        context: &Context,
        worker_index: usize,
        now: &mut Option<Instant>,
        timeout: &mut Option<Duration>,
    ) -> Option<Runnable> {
        let producer = context.producer.borrow();
        let producer = producer.as_ref().unwrap();
        let executor = &context.executor;

        let be_fair = self.tick % 61 == 0;
        if be_fair {
            if let Some(runnable) = self.poll_io(context) {
                return Some(runnable);
            }

            if let Some(runnable) = producer.consume(&executor.injector).success() {
                return Some(runnable);
            }
        }

        if let Some(runnable) = self.poll_timers(context, worker_index, now, timeout) {
            return Some(runnable);
        }

        if let Some(runnable) = producer.pop(be_fair) {
            return Some(runnable);
        }

        self.searching = self.searching || executor.search_begin();
        if self.searching {
            if let Some(runnable) = self.poll_io(context) {
                return Some(runnable);
            }

            const MAX_ATTEMPTS: usize = 32;
            const MAX_RETRIES: usize = 4;

            let mut retries = MAX_RETRIES;
            for attempt in 0..MAX_ATTEMPTS {
                let mut was_contended = match producer.consume(&executor.injector) {
                    Steal::Success(runnable) => return Some(runnable),
                    Steal::Empty => false,
                    Steal::Retry => true,
                };

                for steal_index in context.rng.borrow_mut().gen_iter(executor.rng_iter_source) {
                    if steal_index == worker_index {
                        continue;
                    }

                    match producer.steal(&executor.workers[steal_index].run_queue) {
                        Steal::Success(runnable) => return Some(runnable),
                        Steal::Retry => was_contended = true,
                        Steal::Empty => {}
                    }

                    if !was_contended && (retries == 1 || attempt == MAX_ATTEMPTS - 1) {
                        if let Some(runnable) = self.poll_timers(context, steal_index, now, timeout)
                        {
                            return Some(runnable);
                        }
                    }
                }

                if was_contended || retries > 0 {
                    retries -= !was_contended as usize;
                    spin_loop();
                    continue;
                }

                break;
            }
        }

        None
    }

    fn poll_io(&mut self, context: &Context) -> Option<Runnable> {
        let mut poller = self.poller.borrow_mut();

        let poll_network_guard = poller.try_poll_network(context, &context.executor.net_poller)?;
        poll_network_guard.poll(Some(Duration::ZERO));

        poller.pop_and_inject(context)
    }

    fn poll_timers(
        &mut self,
        context: &Context,
        worker_index: usize,
        now: &mut Option<Instant>,
        timeout: &mut Option<Duration>,
    ) -> Option<Runnable> {
        let mut poller = self.poller.borrow_mut();

        let timer_queue = &context.executor.workers[worker_index].timer_queue;
        poller.poll_timers(context, timer_queue, now, timeout);

        poller.pop_and_inject(context)
    }
}
