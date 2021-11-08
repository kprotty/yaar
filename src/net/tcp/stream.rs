use crate::io::{pollable::Pollable, waker::WakerKind};
use super::{ReadHalf, WriteHalf, OwnedReadHalf, OwnedWriteHalf};
use std::{
    fmt,
    net,
    io::{self, Read, Write},
    pin::Pin,
    task::{Context, Poll},
};

pub struct TcpStream {
    pub(super) pollable: Pollable<mio::net::TcpStream>,
}

impl fmt::Debug for TcpStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpStream").finish()
    }
}

impl TryFrom<net::TcpStream> for TcpStream {
    type Error = io::Error;

    fn try_from(stream: net::TcpStream) -> io::Result<Self> {
        Self::from_std(stream)
    }
}

impl TcpStream {
    pub(super) fn new(stream: mio::net::TcpStream) -> io::Result<Self> {
        Pollable::new(stream).map(|pollable| Self { pollable })
    }

    pub fn from_std(stream: net::TcpStream) -> io::Result<Self> {
        Self::new(mio::net::TcpStream::from_std(stream))
    }

    pub async fn connect(addr: net::SocketAddr) -> io::Result<Self> {
        let stream = mio::net::TcpStream::connect(addr)?;
        let stream = Self::new(stream)?;

        stream.writable().await?;

        match stream.pollable.as_ref().take_error()? {
            Some(error) => Err(error),
            None => Ok(stream),
        }
    }

    pub fn split<'a>(&'a self) -> (ReadHalf<'a>, WriteHalf<'a>) {
        super::split::split(self)
    }

    pub fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
        super::split_owned::split_owned(self)
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

    pub fn local_addr(&self) -> io::Result<net::SocketAddr> {
        self.pollable.as_ref().local_addr()
    }

    pub fn peer_addr(&self) -> io::Result<net::SocketAddr> {
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
        self.poll_peek_inner(ctx, buf)
    }

    pub(super) fn poll_peek_inner(
        &self,
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

    pub fn poll_read_ready(&self, ctx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.pollable.poll_io(WakerKind::Read, Some(ctx), || Ok(()))
    }

    pub fn poll_write_ready(&self, ctx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.pollable
            .poll_io(WakerKind::Write, Some(ctx), || Ok(()))
    }

    pub(super) fn poll_read_inner(
        &self,
        ctx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.pollable.poll_io(WakerKind::Read, Some(ctx), || {
            const TLS_BUF_SIZE: usize = 64 * 1024;
            thread_local!(static TLS_BUF: RefCell<[u8; TLS_BUF_SIZE]> = RefCell::new([0; TLS_BUF_SIZE]));
        
            TLS_BUF.with(|tls_buf| {
                let mut tls_buf = tls_buf.borrow_mut();
                let read_buf = &mut tls_buf[..buf.remaining()];
        
                self.pollable.as_ref().read(read_buf).map(|bytes| {
                    buf.put_slice(&mut read_buf[0..bytes]);
                })
            })
        })
    }

    pub(super) fn poll_write_inner(
        &self,
        ctx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.pollable.poll_io(WakerKind::Write, Some(ctx), || {
            self.pollable.as_ref().write(buf)
        })
    }

    pub(super) fn poll_write_vectored_inner(
        &self,
        ctx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.pollable.poll_io(WakerKind::Write, Some(ctx), || {
            self.pollable.as_ref().write_vectored(bufs)
        })
    }

    pub(super) fn poll_flush_inner(&self) -> Poll<io::Result<()>> {
        // self.pollable.as_ref().flush()?;
        Poll::Ready(Ok(()))
    }

    pub(super) fn poll_shutdown_inner(&self) -> Poll<io::Result<()>> {
        self.pollable.as_ref().shutdown(net::Shutdown::Write)?;
        Poll::Ready(Ok(()))
    }
}

impl tokio::io::AsyncRead for TcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.poll_read_inner(ctx, buf)
    }
}

impl tokio::io::AsyncWrite for TcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.poll_write_inner(ctx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.poll_write_vectored_inner(ctx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush_inner()
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_shutdown_inner()
    }
}
