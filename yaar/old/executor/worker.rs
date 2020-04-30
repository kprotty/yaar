use super::{Platform, Node, Task};
use core::{
    ptr::NonNull,
    cell::Cell,
    mem::MaybeUninit,
    sync::atomic::{Ordering, AtomicUsize},
};

pub struct Worker<P: Platform> {
    pub data: P::WorkerData,
    pub node: NonNull<Node<P>>,
    pub(crate) next: Cell<Option<NonNull<Self>>>,
    run_queue: LocalQueue,
}

/*
GlobalQueue:
    head: ptr = &stub
    tail: ptr = &stub
    stub: ptr = null
    local: ptr = null
    lock: bitset = 0

GlobalQueue::push(front, back):
    back.next = null
    prev = swap(&head, back, acq_rel)
    store(&prev.next, front, rel)
    prev

GlobalQueue::pop(local, lq, max):
    t = null
    if lock.try_acquire():
        while max != 0:
            if local == null:
                local = stub.next.load(acq)
                if local == null:
                    break
                stub.next = null
                h = swap(&head, stub, acq_rel)
                h.next = null
            lq.push(local)
            local = local.next
            max--
        lock.release()
    return t

*/

struct LocalQueue {
    head: AtomicUsize,
    tail: AtomicUsize,
    array: MaybeUninit<[NonNull<Task>; Self::SIZE]>,
}

impl LocalQueue {
    const SIZE: usize = 256;

    fn new() -> Self {
        Self {
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            array: MaybeUninit::uninit(),
        }
    }

    unsafe fn push(
        &self,
        task: NonNull<Task>,
    ) {

    }
}