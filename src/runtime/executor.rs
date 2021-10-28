use super::{
    queue::Queue,
    task::{TaskPoll, TaskScheduled},
};
use std::{
    sync::atomic::{fence, Ordering},
    sync::Arc,
};

pub struct Worker {
    run_queue: Queue,
}

pub struct Executor {
    injected: Queue,
    workers: Box<[Worker]>,
}

impl Executor {
    fn notify(&self) {}

    fn shutdown(&self) {}
}

pub struct ExecutorRef {
    executor: Arc<Executor>,
}

impl Drop for ExecutorRef {
    fn drop(&mut self) {
        self.executor.shutdown()
    }
}

impl AsRef<Executor> for ExecutorRef {
    fn as_ref(&self) -> &Executor {
        self.executor.as_ref()
    }
}

impl ExecutorRef {
    pub fn schedule(self: &Arc<Self>, scheduled: TaskScheduled) {
        let executor: &Executor = self.executor.as_ref();
        let (task, worker_index, _be_fair) = match scheduled {
            TaskScheduled::Spawned { task, worker_index } => (task, Some(worker_index), false),
            TaskScheduled::Awoken { task, worker_index } => (task, Some(worker_index), false),
            TaskScheduled::Yielded { task, worker_index } => (task, Some(worker_index), true),
            TaskScheduled::Injected { task } => (task, None, true),
        };

        match worker_index {
            Some(index) => executor.workers[index].run_queue.push(task),
            None => {
                executor.injected.push(task);
                fence(Ordering::SeqCst);
            }
        }

        executor.notify();
    }
}
