use core::cell::UnsafeCell;

pub static CURRENT: Option<UnsafeCell<dyn Reactor>> = None;

pub trait Reactor {
    type Tick;
    type Event;
    type Buffer;
    type Handle;
    
    fn poll(&self, events: &[Event], timeout: Option<Tick>) -> &[Event];
}