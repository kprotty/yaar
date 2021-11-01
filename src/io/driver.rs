use std::{io, time::Duration};

pub struct Driver {}

impl Driver {
    pub fn new() -> io::Result<Self> {}

    pub fn notify(&self) {}
}

pub struct Poller {}

impl Poller {
    pub fn poll(&mut self, driver: &Driver, timeout: Option<Duration>) {}
}
