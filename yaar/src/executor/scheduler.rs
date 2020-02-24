use super::{Node, Platform, Task, Thread, TaskList};
use core::{ptr::NonNull, slice::from_raw_parts, sync::atomic::{Ordering, AtomicUsize}};
use yaar_lock::ThreadEvent;

pub enum RunError {
    EmptyNodes,
    InvalidStartNode,
    NodeWithoutWorkers(usize),
}

pub struct Scheduler<P: Platform> {
    platform: NonNull<P>,
    nodes_ptr: NonNull<NonNull<Node<P>>>,
    nodes_len: usize,
    pub(crate) next_node: AtomicUsize,
    pub(crate) active_threads: AtomicUsize,
    pub(crate) stop_event: P::ThreadEvent,
}

unsafe impl<P: Platform> Sync for Scheduler<P> {}

impl<P: Platform> Scheduler<P> {
    #[inline]
    pub fn platform(&self) -> &P {
        unsafe { self.platform.as_ref() }
    }

    #[inline]
    pub fn nodes(&self) -> &[&Node<P>] {
        unsafe { from_raw_parts(self.nodes_ptr.as_ptr() as *const _, self.nodes_len) }
    }

    pub(crate) fn schedule(&self, task: NonNull<Task>) {
        if let Some(thread) = Thread::<P>::current() {
            thread.schedule(task);
        }
    }

    pub fn run(
        task: &Task,
        platform: &P,
        start_node: usize,
        nodes: &[NonNull<Node<P>>],
    ) -> Result<(), RunError> {
        if nodes.len() == 0 {
            return Err(RunError::EmptyNodes);
        } else if start_node >= nodes.len() {
            return Err(RunError::InvalidStartNode);
        }

        // Create the scheduler on the stack so that it shares the lifetime
        // of both the nodes slice as well as the platform, this allows the
        // helper functions (`platform()`, `nodes()`) to be safe.
        let this = Self {
            platform: NonNull::new(platform as *const _ as *mut _).unwrap(),
            nodes_ptr: NonNull::new(nodes.as_ptr() as *mut _).unwrap(),
            nodes_len: nodes.len(),
            next_node: AtomicUsize::new(0),
            active_threads: AtomicUsize::new(0),
            stop_event: P::ThreadEvent::default(),
        };

        let scheduler = NonNull::new(&this as *const _ as *mut _).unwrap();
        if nodes.len() == 1 && this.nodes()[0].workers().len() == 1 {
            unimplemented!("TODO: handle specialization for single threaded execution");
        }

        for (node_index, node) in this.nodes().iter().enumerate() {
            node.scheduler.set(Some(scheduler.clone()));
            if node.workers().len() == 0 {
                return Err(RunError::NodeWithoutWorkers(node_index));
            }
            let mut pool = node.pool.lock();
            for worker in node.workers() {
                worker.node.set(NonNull::new(node as *const _ as *mut _));
                pool.put_worker(node, worker);
            }
        }

        // Prepare the task to be executed on the start node
        let node = this.nodes()[start_node];
        node.run_queue.push(unsafe {
            let task = NonNull::new_unchecked(task as *const _ as *mut _);
            let mut list = TaskList::default();
            list.push(task);
            list
        });

        // Run a worker using the main thread to execute the task
        Thread::<P>::run({
            let mut pool = node.pool.lock();
            pool.free_threads -= 1;
            pool.find_worker(node).unwrap()
        });

        // Wait for all spawned threads to come to a halt before exitting. 
        this.stop_event.wait();
        debug_assert_eq!(this.active_threads.load(Ordering::Relaxed), 0);
        Ok(())
    }
}
