use crate::io::{pollable::Pollable, waker::WakerKind};
use std::{
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr},
    pin::Pin,
    task::{Context, Poll},
};

pub struct TcpStream {
    pollable: Pollable<mio::net::TcpStream>,
}

impl TcpStream {
    pub(super) fn new(stream: mio::net::TcpStream) -> io::Result<Self> {
        Pollable::new(stream).map(|pollable| Self { pollable })
    }

    pub async fn connect(addr: SocketAddr) -> io::Result<Self> {
        let stream = mio::net::TcpStream::connect(addr)?;
        let stream = Self::new(stream)?;

        stream.writable().await?;

        match stream.pollable.as_ref().take_error()? {
            Some(error) => Err(error),
            None => Ok(stream),
        }
    }

    pub fn nodelay(&self) -> io::Result<bool> {
        self.pollable.as_ref().nodelay()
    }

    pub fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        self.pollable.as_ref().set_nodelay(nodelay)
    }

    pub fn ttl(&self) -> io::Result<u32> {
        self.pollable.as_ref().ttl()
    }

    pub fn set_ttl(&self, ttl: u32) -> io::Result<()> {
        self.pollable.as_ref().set_ttl(ttl)
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.pollable.as_ref().local_addr()
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.pollable.as_ref().peer_addr()
    }

    pub async fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.pollable
            .poll_future(WakerKind::Read, || self.pollable.as_ref().peek(buf))
            .await
    }

    pub fn poll_peek(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<usize>> {
        let bytes = match self.pollable.poll_io(WakerKind::Read, Some(ctx), || {
            let buf = buf.initialize_unfilled();
            self.pollable.as_ref().peek(buf)
        }) {
            Poll::Ready(bytes) => bytes,
            Poll::Pending => return Poll::Pending,
        };

        if let Ok(bytes) = bytes.as_ref() {
            buf.advance(*bytes);
        }

        Poll::Ready(bytes)
    }

    pub fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.pollable
            .try_io(WakerKind::Read, || self.pollable.as_ref().read(buf))
    }

    pub fn try_read_vectored(&self, bufs: &mut [io::IoSliceMut<'_>]) -> io::Result<usize> {
        self.pollable.try_io(WakerKind::Read, || {
            self.pollable.as_ref().read_vectored(bufs)
        })
    }

    pub fn try_write(&self, buf: &[u8]) -> io::Result<usize> {
        self.pollable
            .try_io(WakerKind::Write, || self.pollable.as_ref().write(buf))
    }

    pub fn try_write_vectored(&self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        self.pollable.try_io(WakerKind::Write, || {
            self.pollable.as_ref().write_vectored(bufs)
        })
    }

    pub async fn readable(&self) -> io::Result<()> {
        self.pollable.poll_future(WakerKind::Read, || Ok(())).await
    }

    pub async fn writable(&self) -> io::Result<()> {
        self.pollable.poll_future(WakerKind::Write, || Ok(())).await
    }
}

impl tokio::io::AsyncRead for TcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let polled = self.pollable.poll_io(WakerKind::Read, Some(ctx), || {
            let buf = buf.initialize_unfilled();
            self.pollable.as_ref().read(buf)
        });

        let bytes = match polled {
            Poll::Ready(bytes) => bytes,
            Poll::Pending => return Poll::Pending,
        };

        let result = bytes.map(|b| buf.advance(b));
        Poll::Ready(result)
    }
}

impl tokio::io::AsyncWrite for TcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.pollable.poll_io(WakerKind::Write, Some(ctx), || {
            self.pollable.as_ref().write(buf)
        })
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.pollable.poll_io(WakerKind::Write, Some(ctx), || {
            self.pollable.as_ref().write_vectored(bufs)
        })
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        // self.pollable.as_ref().flush()?;
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.pollable.as_ref().shutdown(Shutdown::Write)?;
        Poll::Ready(Ok(()))
    }
}
