use crate::io::{
    driver::{PollFairness, Pollable},
    wakers::WakerKind,
};
use std::{
    cell::RefCell,
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr},
    pin::Pin,
    task::{ready, Context, Poll},
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
            .poll_io(WakerKind::Read, ctx.waker(), || self.try_accept())
    }

    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        self.pollable
            .poll_future(WakerKind::Read, || self.try_accept())
            .await
    }

    pub fn try_accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        self.pollable
            .as_ref()
            .accept()
            .and_then(|(stream, addr)| TcpStream::new(stream).map(|stream| (stream, addr)))
    }
}

pub struct TcpStream {
    pollable: Pollable<mio::net::TcpStream>,
    read_fairness: RefCell<PollFairness>,
}

impl TcpStream {
    fn new(stream: mio::net::TcpStream) -> io::Result<Self> {
        Pollable::new(stream).map(|pollable| Self {
            pollable,
            read_fairness: RefCell::new(PollFairness::default()),
        })
    }

    pub async fn connect(addr: impl Into<SocketAddr>) -> io::Result<Self> {
        let addr = addr.into();
        let stream = mio::net::TcpStream::connect(addr)?;
        let this = Self::new(stream)?;

        this.pollable
            .poll_future(WakerKind::Write, || Ok(()))
            .await
            .unwrap();

        match this.pollable.as_ref().take_error()? {
            Some(error) => Err(error),
            None => Ok(this),
        }
    }
}

impl tokio::io::AsyncRead for TcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let polled = self.read_fairness.borrow_mut().poll_fair(ctx.waker(), || {
            self.pollable.poll_io(WakerKind::Read, ctx.waker(), || {
                let buf = buf.initialize_unfilled();
                self.pollable.as_ref().read(buf)
            })
        });

        let bytes = ready!(polled);
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
        self.pollable.poll_io(WakerKind::Write, ctx.waker(), || {
            self.pollable.as_ref().write(buf)
        })
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.pollable.poll_io(WakerKind::Write, ctx.waker(), || {
            self.pollable.as_ref().write_vectored(bufs)
        })
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.pollable.as_ref().flush()?;
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.pollable.as_ref().shutdown(Shutdown::Write)?;
        Poll::Ready(Ok(()))
    }
}

impl TcpStream {
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

    pub fn poll_peek(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<usize>> {
        let polled = self.pollable.poll_io(WakerKind::Read, ctx.waker(), || {
            let buf = buf.initialize_unfilled();
            self.pollable.as_ref().peek(buf)
        });

        let bytes = ready!(polled);
        if let Ok(bytes) = bytes.as_ref() {
            buf.advance(*bytes);
        }

        Poll::Ready(bytes)
    }

    pub async fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.pollable
            .poll_future(WakerKind::Read, || self.pollable.as_ref().peek(buf))
            .await
    }
}
