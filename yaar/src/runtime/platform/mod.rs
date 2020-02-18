use super::scheduler::Worker;
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

    fn tls_slot(new_value: Option<usize>) -> 

    fn spawn_thread(
        &self,
        worker: &Worker<Self>,
        affinity: &Option<Self::CpuAffinity>,
        parameter: usize,
        f: extern "C" fn(param: usize),
    ) -> bool;

    fn on_system_event(&self, _event: SystemEvent<Self>) {}
}

pub enum SystemEvent<P: Platform> {

}
