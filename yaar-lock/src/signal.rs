

pub enum RawSignalState {
    Uninit,
    Retry,
    Waiting,
}

pub enum RawSignalWaitResult {
    Notified,
    Cancelled,
}

pub trait RawSignalEvent {
    fn state(&self) -> RawSignalState;

    fn lazy_init(&self);

    fn notify(&self);

    fn wait(&self, waker: &Waker) -> Poll<RawSignalWaitResult>;
}

pub struct RawSignal<E, S> {

}

impl<E, S> RawSignal<E, S> 
where
    E: AutoResetEvent,
    S: RawSignalEvent,
{
    pub fn new() -> Self {

    }

    pub fn with_notification() -> Self {

    }

    pub fn try_wait(&self) -> bool {

    }

    pub fn wait_fast(&self) -> bool {

    }

    pub fn wait_slow(&self) -> RawSignalWaitFuture<'_, E, S> {
        RawSignalWaitFuture::new(self)
    }

    pub fn notify_one(&self) {

    }

    pub fn notify_all(&self) {

    }
}

impl<'a, E, S> Future for RawSignalWaitFuture<'a, E, S>
where
    E: AutoResetEvent,
    S: RawSignalEvent,
{
    type Output = RawSignalWaitResult;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {

    }
}