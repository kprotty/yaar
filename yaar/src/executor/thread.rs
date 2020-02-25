use super::{Node, Platform, TaggedPtr, Task, TaskScope, TaskList, Worker, Scheduler};
use core::{
    cell::Cell,
    num::NonZeroUsize,
    ptr::{null, NonNull},
    sync::atomic::{compiler_fence, AtomicUsize, Ordering},
    hint::unreachable_unchecked,
};
use yaar_lock::ThreadEvent;

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ThreadState {
    Idle = 0,
    Searching = 1,
    Running = 2,
    Terminating = 3,
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
            1 => ThreadState::Searching,
            2 => ThreadState::Running,
            3 => ThreadState::Terminating,
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
        self.unsync_load_worker_state().as_tag()
    }

    #[inline]
    pub fn worker(&self) -> Option<&Worker<P>> {
        self.unsync_load_worker_state()
            .as_ptr()
            .map(|p| unsafe { &*p.as_ptr() })
    }

    fn unsync_load_worker_state(&self) -> TaggedWorker<P> {
        TaggedWorker::<P>::from(unsafe {
            *(&self.worker_state as *const _ as *const usize)
        })
    }

    pub(crate) fn update_worker_state(&self, scheduler: &Scheduler<P>, new_state: TaggedWorker<P>) {
        let old_state = self.state();
        scheduler.platform().on_thread_state_change(self, old_state, new_state.as_tag());
        self.worker_state.store(new_state.into(), Ordering::Relaxed);
        compiler_fence(Ordering::Release);
    }

    pub(crate) fn schedule(&self, task: NonNull<Task>) {
        unsafe {
            let worker = self.worker().unwrap_or_else(|| unreachable_unchecked());
            let node = worker.node().unwrap_or_else(|| unreachable_unchecked());
            let scheduler = node.scheduler().unwrap_or_else(|| unreachable_unchecked());

            let task_ref = task.as_ref();
            scheduler.platform().on_task_schedule(task_ref);
            match task_ref.scope() {
                TaskScope::Local => {
                    worker.run_queue.push(task, &node.run_queue);
                },
                TaskScope::Global => {
                    // same as Local but may spawn another worker thread to
                    // steal the task from this worker and resume it in parallel.
                    worker.run_queue.push(task, &node.run_queue);
                    if node.workers_searching.load(Ordering::Relaxed) == 0 {
                        node.spawn_worker();
                    }
                },
                TaskScope::System => {
                    // distribute round-robin across all nodes for now
                    let nodes = scheduler.nodes();
                    let node_index = scheduler.next_node.fetch_add(1, Ordering::Relaxed);

                    // fast version of: node_index = node_index % nodes.len() 
                    let num_nodes = NonZeroUsize::new_unchecked(nodes.len());
                    let msb = (!0usize).count_ones() - num_nodes.get().leading_zeros();
                    let node_index = node_index & ((1usize << msb) - 1);

                    // push task to the global queue of the next node
                    let remote_node = nodes.get_unchecked(node_index);
                    remote_node.run_queue.push({
                        let mut list = TaskList::default();
                        list.push(task);
                        list
                    });

                    // make sure theres a worker thread to run the task on that node
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

    pub(crate) extern "C" fn start(worker: &Worker<P>) {
        let node = worker.node().unwrap_or_else(|| unsafe { unreachable_unchecked() });
        let scheduler = node.scheduler().unwrap_or_else(|| unsafe { unreachable_unchecked() });
        let state = match node.workers_searching.load(Ordering::Relaxed) {
            0 => ThreadState::Running,
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
        while let Some(task) = this.poll(scheduler, node, tick) {
            // Transition into running after finding a runnable task.
            // If we we're the last to come out of searching, spawn another worker to handle future incoming tasks as soon as possible for maximum worker utilization.
            let old_state = this.unsync_load_worker_state();
            let new_state = old_state.with_tag(ThreadState::Running);
            this.update_worker_state(scheduler, new_state);
            if old_state.as_tag() == ThreadState::Searching {
                if node.workers_searching.fetch_sub(1, Ordering::Release) == 1 {
                    node.spawn_worker();
                }
            }
            
            // Resumes the task and advances a local schedule tick
            tick += 1;
            unsafe {
                let task = task.as_ref();
                scheduler.platform().on_task_resume(task);
                task.resume();
            }
        }

        // Prepare the thread to terminate.
        let old_state = this.unsync_load_worker_state();
        debug_assert_eq!(old_state.as_tag(), ThreadState::Running);
        let new_state = TaggedWorker::new(null(), ThreadState::Terminating);
        this.update_worker_state(scheduler, new_state);

        // Update the free_thread count to let another thread spawn.
        let was_last_thread = {
            let mut pool = node.pool.lock();
            debug_assert!(pool.free_threads < pool.max_threads.get());
            pool.free_threads += 1;
            pool.free_threads == pool.max_threads.get()
        };

        // If this was the last thread in the node to terminate,
        // notify the scheduler and signal to stop if this was the
        // last node with a thread.
        if was_last_thread {
            let active_nodes = scheduler.active_nodes.fetch_sub(1, Ordering::Relaxed);
            debug_assert!(active_nodes > 0 && active_nodes <= scheduler.nodes().len());
            scheduler.platform().on_node_stop(node);
            if active_nodes == 1 {
                scheduler.stop_event.set();
            }
        }
    }

    fn poll(&self, scheduler: &Scheduler<P>, node: &Node<P>, tick: usize) -> Option<NonNull<Task>> {
        loop {
            // Cant poll for tasks without a worker
            let worker = match self.worker() {
                Some(worker) => worker,
                None => return None,
            };

            // When giving up worker:
            worker
                .last_thread
                .set(NonNull::new(self as *const _ as *mut _));
            node.pool.lock().put_worker(node, worker);

            unimplemented!()
        }
    }
}
