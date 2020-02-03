use yaar_lock::ThreadEvent;

pub trait Platform: Sync {
    type SpawnError;
    type CpuAffinity;
    type ThreadLocal: ThreadLocal;
    type ThreadEvent: ThreadEvent;

    fn spawn_thread(
        &self,
        node_id: usize,
        worker_id: usize,
        affinity: &Self::CpuAffinity,
        parameter: usize,
        f: extern "C" fn(param: usize),
    ) -> Result<(), Self::SpawnError>;
}

pub trait ThreadLocal {
    fn get(&self) -> usize;

    fn set(&self, value: usize);
}
