
use super::platform::Platform;
use super::handler::EventHandler;

use core::{
    future::Future,
    num::NonZeroUsize,
    alloc::GlobalAlloc,
};

/// Used to configure and run futures through the Zap Runtime.
///
/// Zap does not assume a default allocator but instead requires
/// the user to pass in one of their choosing known at compile time.
///
/// Zap also allows hooking onto runtime events through the [`EventHandler`] trait.
/// Example use-cases include profiling, logging, and debugging.
///
/// Methods can be chained in order to set configuration values. Once the instance of
/// `Config` is ready, the [`run`] function can be used to execute a future along with
/// any others it spawns using the Zap runtime configured with the set options.
pub struct Config<P, A, H> {
    platform: P,
    allocator: A,
    event_handler: H,
    max_threads: NonZeroUsize,
    max_async_threads: NonZeroUsize,
    thread_stack_size: Option<NonZeroUsize>,
}

impl<P, A, H> Config<P, A, H> 
where
    P: Platform,
{
    /// Returns a runtime config initialized with the default values
    /// excluding the user provided allocator and event handler.
    ///
    /// Configuration methods can be chained on the return value.
    pub fn new(platform: P, allocator: A, event_handler: H) -> Self {
        Self {
            platform,
            allocator,
            event_handler,
            max_threads: NonZeroUsize::new(512).unwrap(),
            max_async_threads: P::num_cpus().unwrap_or(NonZeroUsize::new(1).unwrap()),
            thread_stack_size: None,
        }
    }
}

impl<P, A, H> Default for Config<P, A, H>
where
    P: Platform + Default,
    A: Default,
    H: Default,
{
    fn default() -> Self {
        Self::new(P::default(), A::default(), H::default())
    }
}

impl<P, A, H> Config<P, A, H> {
    /// Set the maximum amount of platform/OS threads,
    /// excluding internal threads, that the runtime is allowed to spawn.
    ///
    /// This limit should generally be not too much for the system to handle
    /// and currently defaults to 512.
    ///
    /// Threads may be spawned to run async Futures or to run blocking operations.
    /// To configure the amount of threads running async Futures, use [`max_async_threads`] instead.
    /// 
    /// Note that `max_async_threads` is limited by `max_threads`.
    pub fn max_threads(&mut self, amount: NonZeroUsize) -> &mut Self {
        self.max_threads = amount;
        self
    }

    /// Set the maximum amount of platform/OS threads running async Futures.
    ///
    /// This limit should strive to match the available concurrency of the system
    /// and currently defaults to the amount of logical cpus reported by the platform.
    ///
    /// Note that this is limited by [`max_threads`].
    pub fn max_async_threads(&mut self, amount: NonZeroUsize) -> &mut Self {
        self.max_threads = amount;
        self
    }

    /// Set a hint for the default stack size of threads spawned by the runtime.
    /// Defaults to `None`
    ///
    /// A value of `None` means the runtime is free to decide on the stack size.
    pub fn thread_stack_size(&mut self, stack_size: Option<NonZeroUsize>) -> &mut Self {
        self.thread_stack_size = stack_size;
        self
    }
}

impl<P, A, H> Config<P, A, H> 
where
    P: Platform,
    A: GlobalAlloc,
    H: EventHandler,
{
    pub fn run<T>(&self, _future: impl Future<Output = T>) -> Result<T, ()> {
        unreachable!("Unimplemented");
    }
}