
#[cfg(feature = "sync")]
mod sync;
#[cfg(feature = "sync")]
pub use self::sync::*;

#[cfg(feature = "async")]
mod futures;
#[cfg(feature = "async")]
pub use self::futures::{
    Mutex as AsyncMutex,
    MutexGuard as AsyncMutexGuard,
};