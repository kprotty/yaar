
pub enum Event {
    ThreadBegin,
    ThreadEnd,
}

pub trait Platform {
    type RawMutex: lock_api::RawMutex;
    type ThreadEvent: yaar_lock::ThreadEvent;

    type WorkerLocalData;
    type ThreadLocalData;
    type NodeLocalData: Sync;

    unsafe fn get_tls() -> usize;

    unsafe fn set_tls(new_value: usize);

    unsafe fn spawn_thread(
        &self,
        worker: &super::Worker<Self>,
        parameter: usize,
        f: extern "C" fn(parameter: usize),
    ) -> bool;

    fn on_event(&self, event: Event) {
        let _ = event;
    }
}
