use super::scheduler::{Node, Worker};
use yaar_lock::ThreadEvent;

#[cfg(feature = "time")]
use super::time::Clock;
#[cfg(feature = "io")]
use yaar_reactor::Reactor;

pub trait Platform: Sync + Sized + 'static {
    type CpuAffinity;
    type ThreadEvent: ThreadEvent;

    #[cfg(feature = "time")]
    type Clock: Clock;
    #[cfg(feature = "io")]
    type Reactor: Reactor;

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
