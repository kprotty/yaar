use super::{Thread, Worker};
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
        run: extern "C" fn(worker: &Worker<Self>),
    ) -> bool;
}
