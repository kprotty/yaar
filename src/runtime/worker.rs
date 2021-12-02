use super::{
    context::Context,
    executor::Executor,
    parker::Parker,
    queue::{Runnable, Steal, Stealer as QueueStealer, Worker as QueueWorker},
    random::Rng,
};
use std::{
    any::Any,
    future::Future,
    hint::spin_loop,
    mem::replace,
    panic,
    pin::Pin,
    ops::{Deref, DerefMut},
    sync::atomic::AtomicUsize,
    sync::{Arc, Mutex},
    task::{Context as PollContext, Poll, Waker},
};

pub struct Worker {
    pub idle_next: AtomicUsize,
    queue_worker: Mutex<Option<QueueWorker>>,
    queue_stealer: QueueStealer,
}

impl Worker {
    pub fn new() -> Self {
        let worker = QueueWorker::new_lifo();
        let stealer = worker.stealer();
        
        Self {
            idle_next: AtomicUsize::new(0),
            queue_worker: Mutex::new(Some(worker)),
            queue_stealer: stealer,
        }
    }

    pub fn block_on<F, P>(mut worker_index: Option<usize>, future: Pin<P>) -> F::Output 
    where
        F: Future,
        P: Deref<Target = F> + DerefMut,
    {
        let mut rng = Rng::new();
        let tick = rng.next();

        let mut worker_context = WorkerContext {
            rng,
            tick,
            searching: false,
            context: Context::enter(),
            parker: Arc::new(Parker::new()),
            future,
            executor: Executor::global(),
        };

        worker_index = worker_index.or_else(|| worker_context.executor.search_retry());
        if let Some(worker_index) = worker_index {
            worker_context.transition_to_notified(worker_index);
        }

        assert!(worker_context.parker.task_state.transition_to_scheduled());
        let result = worker_context.block_on();

        if let Some(worker_index) = worker_context.context.worker_index.get() {
            worker_context.transition_to_idle(worker_index);
        }

        drop(worker_context);
        match result {
            Ok(output) => output,
            Err(error) => panic::resume_unwind(error),
        }
    }
}

struct WorkerContext<F, P> 
where
    F: Future,
    P: Deref<Target = F> + DerefMut,
{
    rng: Rng,
    tick: usize,
    searching: bool,
    context: Context,
    parker: Arc<Parker>,
    future: Pin<P>,
    executor: &'static Executor,
}

impl<F, P> WorkerContext<F, P>
where
F: Future,
P: Deref<Target = F> + DerefMut,
{
    fn transition_to_notified(&mut self, worker_index: usize) {
        assert_eq!(self.context.worker_index.get(), None);
        self.context.worker_index.set(Some(worker_index));

        let queue_worker_slot = &self.executor.workers[worker_index].queue_worker;
        let queue_worker = replace(&mut *queue_worker_slot.try_lock().unwrap(), None);
        let queue_worker = queue_worker.unwrap();

        let queue_worker = self.context.queue_worker.borrow_mut().replace(queue_worker);
        assert!(queue_worker.is_none());

        let was_searching = replace(&mut self.searching, true);
        assert!(!was_searching);
    }

    fn transition_to_idle(&mut self, worker_index: usize) -> bool {
        assert_eq!(self.context.worker_index.get(), Some(worker_index));
        self.context.worker_index.set(None);

        let queue_worker = self.context.queue_worker.borrow_mut().take();
        assert!(queue_worker.is_some());

        let queue_worker_slot = &self.executor.workers[worker_index].queue_worker;
        let queue_worker = replace(&mut *queue_worker_slot.try_lock().unwrap(), queue_worker);
        assert!(queue_worker.is_none());

        let was_searching = replace(&mut self.searching, false);
        self.executor.search_failed(was_searching, worker_index)
    }

    fn block_on(&mut self) -> Result<F::Output, Box<dyn Any + Send + 'static>> {
        loop {
            let poll_retry = match self.poll_future() {
                Poll::Ready(Some(result)) => return result,
                Poll::Ready(None) => true,
                Poll::Pending => false,
            };

            if let Some(runnable) = self.poll_runnable() {
                self.tick = self.tick.wrapping_add(1);
                if replace(&mut self.searching, false) {
                    self.executor.search_discovered();
                }

                runnable.run(&self.context);
                continue;
            }

            if poll_retry {
                continue;
            }

            if let Some(worker_index) = self.executor.park(&self.parker) {
                self.transition_to_notified(worker_index);
            }
        }
    }

    fn poll_future(&mut self) -> Poll<Option<Result<F::Output, Box<dyn Any + Send + 'static>>>> {
        if !self
            .parker
            .task_state
            .transition_to_running_from_scheduled()
        {
            return Poll::Pending;
        }

        match {
            let waker = Waker::from(self.parker.clone());
            let mut ctx = PollContext::from_waker(&waker);
            panic::catch_unwind(panic::AssertUnwindSafe(|| {
                self.future.as_mut().poll(&mut ctx)
            }))
        } {
            Err(error) => Poll::Ready(Some(Err(error))),
            Ok(Poll::Ready(output)) => Poll::Ready(Some(Ok(output))),
            Ok(Poll::Pending) => {
                if self.parker.task_state.transition_to_idle() {
                    return Poll::Pending;
                }

                assert!(self.parker.task_state.transition_to_running_from_notified());
                Poll::Ready(None)
            }
        }
    }

    fn poll_runnable(&mut self) -> Option<Runnable> {
        loop {
            let worker_index = self.context.worker_index.get()?;
            let mut queue_worker_rc = self.context.queue_worker.borrow_mut();
            let queue_worker = queue_worker_rc.as_mut().unwrap();

            if self.tick % 61 == 0 {
                if let Steal::Success(runnable) = self.executor.injector.steal() {
                    return Some(runnable);
                }

                let stealer = &self.executor.workers[worker_index].queue_stealer;
                if let Steal::Success(runnable) = stealer.steal() {
                    return Some(runnable);
                }
            }

            if let Some(runnable) = queue_worker.pop() {
                return Some(runnable);
            }

            self.searching = self.searching || self.executor.search_begin();
            if self.searching {
                for _attempt in 0..32 {
                    let mut retry = match self.executor.injector.steal_batch_and_pop(queue_worker) {
                        Steal::Success(runnable) => return Some(runnable),
                        Steal::Retry => true,
                        Steal::Empty => false,
                    };

                    for steal_index in self.rng.seq(self.executor.rng_seq_seed) {
                        if steal_index == worker_index {
                            continue;
                        }

                        let stealer = &self.executor.workers[steal_index].queue_stealer;
                        match stealer.steal_batch_and_pop(queue_worker) {
                            Steal::Success(runnable) => return Some(runnable),
                            Steal::Retry => retry = true,
                            Steal::Empty => {}
                        }
                    }

                    if retry {
                        spin_loop();
                    } else {
                        break;
                    }
                }
            }

            drop(queue_worker);
            drop(queue_worker_rc);

            if self.transition_to_idle(worker_index) {
                if let Some(worker_index) = self.executor.search_retry() {
                    self.transition_to_notified(worker_index);
                    continue;
                }
            }

            return None;
        }
    }
}
