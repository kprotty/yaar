use super::stream::TcpStream;
use std::{
    fmt, io, net,
    pin::Pin,
    task::{Context, Poll},
};

pub(super) fn split<'a>(stream: &'a TcpStream) -> (ReadHalf<'a>, WriteHalf<'a>) {
    let read_half = ReadHalf { stream };
    let write_half = WriteHalf { stream };
    (read_half, write_half)
}

pub struct ReadHalf<'a> {
    stream: &'a TcpStream,
}

impl<'a> fmt::Debug for ReadHalf<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadHalf").finish()
    }
}

impl<'a> ReadHalf<'a> {
    pub fn local_addr(&self) -> io::Result<net::SocketAddr> {
        self.stream.local_addr()
    }

    pub fn peer_addr(&self) -> io::Result<net::SocketAddr> {
        self.stream.peer_addr()
    }

    pub async fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.peek(buf).await
    }

    pub fn poll_peek(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<usize>> {
        self.stream.poll_peek_inner(ctx, buf)
    }

    pub fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.try_read(buf)
    }

    pub fn try_read_vectored(&self, bufs: &mut [io::IoSliceMut<'_>]) -> io::Result<usize> {
        self.stream.try_read_vectored(bufs)
    }

    pub async fn readable(&self) -> io::Result<()> {
        self.stream.readable().await
    }
}

impl<'a> AsRef<TcpStream> for ReadHalf<'a> {
    fn as_ref(&self) -> &TcpStream {
        self.stream
    }
}

impl<'a> tokio::io::AsyncRead for ReadHalf<'a> {
    fn poll_read(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.stream.poll_read_inner(ctx, buf)
    }
}

pub struct WriteHalf<'a> {
    stream: &'a TcpStream,
}

impl<'a> fmt::Debug for WriteHalf<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnedWriteHalf").finish()
    }
}

impl<'a> WriteHalf<'a> {
    pub fn local_addr(&self) -> io::Result<net::SocketAddr> {
        self.stream.local_addr()
    }

    pub fn peer_addr(&self) -> io::Result<net::SocketAddr> {
        self.stream.peer_addr()
    }

    pub fn try_write(&self, buf: &[u8]) -> io::Result<usize> {
        self.stream.try_write(buf)
    }

    pub fn try_write_vectored(&self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        self.stream.try_write_vectored(bufs)
    }

    pub async fn writable(&self) -> io::Result<()> {
        self.stream.writable().await
    }
}

impl<'a> AsRef<TcpStream> for WriteHalf<'a> {
    fn as_ref(&self) -> &TcpStream {
        self.stream
    }
}

impl<'a> tokio::io::AsyncWrite for WriteHalf<'a> {
    fn poll_write(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.stream.poll_write_inner(ctx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.stream.poll_write_vectored_inner(ctx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.stream.poll_flush_inner()
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.stream.poll_shutdown_inner()
    }
}
