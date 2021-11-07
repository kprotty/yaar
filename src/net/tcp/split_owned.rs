use super::stream::TcpStream;
use std::{fmt, io, net, error, mem::drop, pin::Pin, task::{Context, Poll}, sync::Arc};

pub(super) fn split_owned(stream: TcpStream) -> (OwnedReadHalf, OwnedWriteHalf) {
    let stream = Arc::new(stream);
    let read_half = OwnedReadHalf {
        stream: stream.clone(),
    };
    let write_half = OwnedWriteHalf {
        stream,
        shutdown_on_drop: true,
    };

    (read_half, write_half)
}

fn reunite(
    read_half: OwnedReadHalf,
    write_half: OwnedWriteHalf,
) -> Result<TcpStream, ReuniteError> {
    if Arc::ptr_eq(&read_half.stream, &write_half.stream) {
        write_half.forget();
        Ok(Arc::try_unwrap(read_half.stream).expect("TcpStream: try_unwrap() failed to reunite"))
    } else {
        Err(ReuniteError(read_half, write_half))
    }
}

#[derive(Debug)]
pub struct ReuniteError(pub OwnedReadHalf, pub OwnedWriteHalf);

impl error::Error for ReuniteError {}

impl fmt::Display for ReuniteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "tried to reunite halves that weren't from the same TcpStream"
        )
    }
}

pub struct OwnedReadHalf {
    pub(super) stream: Arc<TcpStream>,
}

impl fmt::Debug for OwnedReadHalf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnedReadHalf").finish()
    }
}

impl OwnedReadHalf {
    pub fn reunite(self, other: OwnedWriteHalf) -> Result<TcpStream, ReuniteError> {
        reunite(self, other)
    }

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

impl AsRef<TcpStream> for OwnedReadHalf {
    fn as_ref(&self) -> &TcpStream {
        self.stream.as_ref()
    }
}

impl tokio::io::AsyncRead for OwnedReadHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.stream.poll_read_inner(ctx, buf)
    }
}

pub struct OwnedWriteHalf {
    pub(super) stream: Arc<TcpStream>,
    shutdown_on_drop: bool,
}

impl Drop for OwnedWriteHalf {
    fn drop(&mut self) {
        if self.shutdown_on_drop {
            assert!(self.stream.poll_shutdown_inner().is_ready());
        }
    }
}

impl fmt::Debug for OwnedWriteHalf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnedWriteHalf").finish()
    }
}

impl OwnedWriteHalf {
    pub fn forget(mut self) {
        self.shutdown_on_drop = false;
        drop(self);
    }

    pub fn reunite(self, other: OwnedReadHalf) -> Result<TcpStream, ReuniteError> {
        reunite(other, self)
    }

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

impl AsRef<TcpStream> for OwnedWriteHalf {
    fn as_ref(&self) -> &TcpStream {
        self.stream.as_ref()
    }
}

impl tokio::io::AsyncWrite for OwnedWriteHalf {
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

    fn poll_shutdown(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = self.stream.poll_shutdown_inner();
        if result.is_ready() {
            self.shutdown_on_drop = false;
        }
        result
    }
}
