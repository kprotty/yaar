mod event;
pub use self::event::Event;
#[cfg(feature = "os")]
pub use self::event::OsEvent;

mod mutex;
#[cfg(feature = "os")]
pub use self::mutex::{Mutex, MutexGuard};
pub use self::mutex::{RawMutex, RawMutexGuard};
