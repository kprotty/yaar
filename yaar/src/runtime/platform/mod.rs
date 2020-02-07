use super::scheduler::{Node, Worker};
use yaar_lock::ThreadEvent;

pub trait Platform: Sync + Sized + 'static {
    type CpuAffinity;
    type ThreadEvent: ThreadEvent;

    type NodeLocalData;
    type WorkerLocalData;
    type ThreadLocalData: Default;

    fn get_tls(&self) -> usize;

    fn set_tls(&self, value: usize);

    fn spawn_thread(
        &self,
        node: &Node<Self>,
        worker: &Worker<Self>,
        affinity: &Option<Self::CpuAffinity>,
        parameter: usize,
        f: extern "C" fn(param: usize),
    ) -> bool;
}
