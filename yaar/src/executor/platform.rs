use super::{Thread, Worker, Node, Task, ThreadState};
use core::ptr::NonNull;

pub unsafe trait Platform: Sized + Sync {
    type RawMutex: lock_api::RawMutex;
    type ThreadEvent: yaar_lock::ThreadEvent;

    type WorkerLocalData;
    type ThreadLocalData: Default;
    type NodeLocalData: Sync;

    fn get_tls_thread() -> Option<NonNull<Thread<Self>>>;

    fn set_tls_thread(thread: Option<NonNull<Thread<Self>>>);

    fn spawn_thread(
        &self,
        worker: &Worker<Self>,
        entry_fn: extern "C" fn(worker: &Worker<Self>),
    ) -> bool;

    fn on_task_schedule(&self, _task: &Task) {}

    fn on_task_resume(&self, _task: &Task) {}

    fn on_node_start(&self, _node: &Node<Self>) {}

    fn on_node_stop(&self, _node: &Node<Self>) {}

    fn on_thread_state_change(
        &self,
        _thread: &Thread<Self>,
        _old_state: ThreadState,
        _new_state: ThreadState,
    ) {}
}
