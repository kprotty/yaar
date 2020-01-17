#[cfg(any(feature = "io", feature = "time"))]
use core::time::Duration;

/// An interface for the runtime executor
/// to perform any platform specific tasks.
pub trait Platform {
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

    /// Get the current timestamp of the internal timer.
    ///
    /// Used for measuring elapsed time and waking expired tasks
    /// so it would be best if the underlying timer is monotonic.
    #[cfg(feature = "time")]
    fn time_now(&self) -> Duration;

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
    fn io_poll(&self, timeout: Option<Duration>) -> usize;
}

// TODO: OsPlatform
