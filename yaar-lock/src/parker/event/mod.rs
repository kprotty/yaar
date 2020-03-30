

pub unsafe trait AutoResetEvent: Sync + Default {    
    fn set(&self);

    fn wait(&self);

    /// Hint to the platform that a thread is spinning, similar to [`spin_loop_hint`].
    ///
    /// Returns whether the thread should continue spinning or not.
    fn yield_now(iteration: usize) -> bool;
}

pub unsafe trait AutoResetEventTimed: AutoResetEvent {
    type Tick;

    fn try_wait(&self, timeout: &mut Self::Tick) -> bool;
}