use super::{Node, Platform, TaggedPtr, Task, TaskScope, TaskList, Worker};
use core::{
    cell::Cell,
    ptr::{null, NonNull},
    sync::atomic::{AtomicUsize, Ordering},
    hint::unreachable_unchecked,
};
use yaar_lock::ThreadEvent;

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ThreadState {
    Idle = 0,
    Polling = 1,
    Searching = 2,
    Running = 3,
}

impl Into<usize> for ThreadState {
    fn into(self) -> usize {
        self as usize
    }
}

impl From<usize> for ThreadState {
    fn from(value: usize) -> Self {
        match value & 0b11 {
            0 => ThreadState::Idle,
            1 => ThreadState::Polling,
            2 => ThreadState::Searching,
            3 => ThreadState::Running,
            _ => unreachable!(),
        }
    }
}

pub(crate) type TaggedWorker<P> = TaggedPtr<Worker<P>, ThreadState>;

pub struct Thread<P: Platform> {
    pub data: P::ThreadLocalData,
    pub(crate) prev: Cell<Option<NonNull<Self>>>,
    pub(crate) next: Cell<Option<NonNull<Self>>>,
    pub(crate) worker_state: AtomicUsize,
    pub(crate) event: P::ThreadEvent,
}

impl<P: Platform> Thread<P> {
    #[inline]
    pub fn current<'a>() -> Option<&'a Self> {
        P::get_tls_thread().map(|p| unsafe { &*p.as_ptr() })
    }

    #[inline]
    pub fn state(&self) -> ThreadState {
        TaggedWorker::<P>::from(self.worker_state.load(Ordering::Relaxed)).as_tag()
    }

    #[inline]
    pub fn worker(&self) -> Option<&Worker<P>> {
        TaggedWorker::from(self.worker_state.load(Ordering::Relaxed))
            .as_ptr()
            .map(|ptr| unsafe { &*ptr.as_ptr() })
    }

    pub(crate) fn schedule(&self, task_ptr: NonNull<Task>) {
        unsafe {
            let task = task_ptr.as_ref();
            let worker = self.worker().unwrap_or_else(|| unreachable_unchecked());
            let node = worker.node().unwrap_or_else(|| unreachable_unchecked());

            match task.scope() {
                TaskScope::Local => {
                    worker.run_queue.push(task, &node.run_queue);
                },
                TaskScope::Global => {
                    worker.run_queue.push(task, &node.run_queue);
                    if node.workers_searching.load(Ordering::Relaxed) == 0 {
                        node.spawn_worker();
                    }
                },
                TaskScope::System => {
                    let scheduler = node.scheduler().unwrap_or_else(|| unreachable_unchecked());
                    let nodes = scheduler.nodes();

                    let node_index = scheduler.next_node.fetch_add(1, Ordering::Relaxed);
                    let remote_node = nodes.get_unchecked(node_index % nodes.len());
                    remote_node.run_queue.push({
                        let mut list = TaskList::default();
                        list.push(task_ptr);
                        list
                    });

                    if remote_node.workers_searching.load(Ordering::Relaxed) == 0 {
                        remote_node.spawn_worker();
                    }
                },
                TaskScope::Remote => {
                    unimplemented!();
                },
            };
        }
    }

    pub(crate) extern "C" fn run(worker: &Worker<P>) {
        let node = worker.node().expect("Thread running without a Node");
        let state = match node.workers_searching.load(Ordering::Relaxed) {
            0 => ThreadState::Idle,
            _ => ThreadState::Searching,
        };

        let this = Self {
            data: P::ThreadLocalData::default(),
            prev: Cell::new(None),
            next: Cell::new(None),
            worker_state: AtomicUsize::new(TaggedWorker::new(worker, state).into()),
            event: P::ThreadEvent::default(),
        };

        let this_ptr = NonNull::new(&this as *const _ as *mut _);
        worker.thread.set(this_ptr);
        P::set_tls_thread(this_ptr);

        let mut tick = 0;
        while let Some(task) = this.poll(node, tick) {
            // Transition into running after finding a runnable task.
            // If we we're the last to come out of searching, spawn
            // another worker to handle future incoming tasks as soon
            // as possible for maximum worker utilization.
            let worker_state = TaggedWorker::<P>::from(this.worker_state.load(Ordering::Relaxed));
            let new_state = worker_state.with_tag(ThreadState::Running);
            this.worker_state.store(new_state.into(), Ordering::Relaxed);
            if worker_state.as_tag() == ThreadState::Searching {
                if node.workers_searching.fetch_sub(1, Ordering::Release) == 1 {
                    node.spawn_worker();
                }
            }

            unsafe { task.as_ref().resume() };
            tick += 1;
        }

        // Thread is exitting, set the stop_event if this was the last thread.
        let scheduler = node
            .scheduler()
            .expect("Thread::run: Node without a scheduler");
        node.pool.lock().free_threads += 1;
        if scheduler.active_threads.fetch_sub(1, Ordering::Relaxed) == 1 {
            scheduler.stop_event.set();
        }
    }

    fn poll(&self, node: &Node<P>, tick: usize) -> Option<NonNull<Task>> {
        loop {
            let worker = match self.worker() {
                Some(worker) => worker,
                None => return None,
            };

            // When giving up worker:
            let new_state = TaggedWorker::<P>::new(null(), ThreadState::Polling);
            self.worker_state.store(new_state.into(), Ordering::Relaxed);
            worker
                .last_thread
                .set(NonNull::new(self as *const _ as *mut _));
            node.pool.lock().put_worker(node, worker);

            unimplemented!()
        }
    }
}
