
/// An interface for the runtime executor
/// to interact with the underlying platform/system.
pub trait Platform {
    /// A [`yaar_lock::sync::ThreadParker`] used for runtime synchronization.
    type Parker;

    /// Each thread has a usize TLS slot associated with it.
    /// Returns the value of the current thread's TLS slot.
    fn get_tls(&self) -> usize;

    /// Each thread has a usize TLS value associated with it.
    /// Set the value of the current thread's TLS slot.
    fn set_tls(&self, value: usize);

    /// Spawn a new thread, which has the ability to run concurrently or
    /// in parallel with any other threads, using the given parameter and
    /// entry point function.
    ///
    /// Returns whether spawning thread was successful.
    fn spawn_thread(&self, parameter: usize, f: extern "C" fn(usize)) -> bool;

    /// Get the current timestamp of the internal timer in milliseconds.
    ///
    /// Used for measuring elapsed time and waking expired tasks
    /// so it would be best if the underlying timer is monotonic.
    #[cfg(feature = "time")]
    fn get_current_tick(&self) -> u64;

    /// Poll the internal IO service for ready futures,
    /// returning the number of futures that were woken up.
    ///
    /// `timeout` specifies a hint for the maximum amount of time the
    /// thread should spend blocking for futures to become ready.
    ///
    /// A timeout of 0 indicates that the poll request shoudl strive
    /// to be non-blocking and return immediately. A timeout of `None`
    /// indicates that the poll request is free to blocking forever
    /// or as long as it wants until a future is woken up.
    #[cfg(feature = "io")]
    fn poll_io(&self, timeout_ticks: Option<u64>) -> usize;
}

// TODO: OsPlatform
