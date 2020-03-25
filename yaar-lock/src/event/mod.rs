

pub unsafe trait AutoResetEvent: Sync {
    fn is_set(&self) -> bool;
    
    fn set(&self);

    fn wait(&self);

    fn yield_now(iteration: usize) -> bool {
        core::sync::atomic::spin_loop_hint();
        false
    }
}

pub unsafe trait AutoResetEventTimed: AutoResetEvent {
    type Instant;
    type Duration;

    fn try_wait_until(&self, timeout: &mut Self::Instant) -> bool;

    fn try_wait_for(&self, timeout: &mut Self::Duration) -> bool;
}