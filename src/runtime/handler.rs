
/// Interface for handling events emitted by the Zap Runtime.
///
/// TODO: add functions
pub trait EventHandler: Send + Sync {
    
}

/// An EventHandler which uses the default functionality.
#[derive(Default, Copy, Clone)]
pub struct DefaultHandler;

impl EventHandler for DefaultHandler {}
