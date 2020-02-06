use yaar_lock::ThreadEvent;

pub trait Platform: Sync {
    type CpuAffinity;
    type ThreadEvent: ThreadEvent;

    fn get_tls(&self) -> usize;

    fn set_tls(&self, value: usize);

    fn spawn_thread(
        &self,
        node_id: usize,
        affinity: &Option<Self::CpuAffinity>,
        parameter: usize,
        f: extern "C" fn(param: usize),
    ) -> bool;
}
