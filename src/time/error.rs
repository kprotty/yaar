use std::{error::Error, fmt, io};

#[derive(Debug, PartialEq)]
pub struct Elapsed(());

impl Elapsed {
    pub(super) fn new() -> Self {
        Elapsed(())
    }
}

impl fmt::Display for Elapsed {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        "deadline has elapsed".fmt(fmt)
    }
}

impl Error for Elapsed {}

impl From<Elapsed> for io::Error {
    fn from(_err: Elapsed) -> io::Error {
        io::ErrorKind::TimedOut.into()
    }
}
