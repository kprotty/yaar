use super::{Node, Platform, Task, Thread};
use core::{ptr::NonNull, slice::from_raw_parts, sync::atomic::AtomicUsize};
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

        let node = this.nodes()[start_node];
        Thread::<P>::run({
            let mut pool = node.pool.lock();
            pool.free_threads -= 1;
            pool.find_worker(node).unwrap()
        });

        this.stop_event.wait();
        Ok(())
    }
}
