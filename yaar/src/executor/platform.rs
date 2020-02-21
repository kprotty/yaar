use super::Worker;
use core::num::NonZeroUsize;

pub trait Platform: Sized {
    type RawMutex: lock_api::RawMutex;
    type ThreadEvent: yaar_lock::ThreadEvent;

    type WorkerLocalData;
    type ThreadLocalData;
    type NodeLocalData: Sync;

    unsafe fn get_tls() -> Option<NonZeroUsize>;

    unsafe fn set_tls(new_value: NonZeroUsize);

    unsafe fn spawn_thread(
        &self,
        worker: &Worker<Self>,
        run: extern "C" fn(worker: &Worker<Self>),
    ) -> bool;
}
