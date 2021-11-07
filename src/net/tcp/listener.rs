use super::stream::TcpStream;
use crate::io::{pollable::Pollable, waker::WakerKind};
use std::{
    io,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

pub struct TcpListener {
    pollable: Pollable<mio::net::TcpListener>,
}

impl TcpListener {
    pub fn bind(addr: impl Into<SocketAddr>) -> io::Result<Self> {
        let addr = addr.into();
        let listener = mio::net::TcpListener::bind(addr)?;
        Pollable::new(listener).map(|pollable| Self { pollable })
    }

    pub fn poll_accept(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
    ) -> Poll<io::Result<(TcpStream, SocketAddr)>> {
        self.pollable
            .poll_io(WakerKind::Read, Some(ctx), || self.try_accept_inner())
    }

    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        self.pollable
            .poll_future(WakerKind::Read, || self.try_accept_inner())
            .await
    }

    pub fn try_accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        self.pollable
            .try_io(WakerKind::Read, || self.try_accept_inner())
    }

    fn try_accept_inner(&self) -> io::Result<(TcpStream, SocketAddr)> {
        self.pollable
            .as_ref()
            .accept()
            .and_then(|(stream, addr)| TcpStream::new(stream).map(|stream| (stream, addr)))
    }
}
