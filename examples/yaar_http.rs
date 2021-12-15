use hyper::{
    server::{conn::Http, Builder},
    service::{make_service_fn, service_fn},
    Body, Request, Response,
};
use std::{convert::Infallible, net::SocketAddr};

async fn hello_world(_: Request<Body>) -> Result<Response<Body>, Infallible> {
    Ok(Response::new(Body::from("Hello World")))
}

pub fn main() -> Result<(), hyper::Error> {
    yaar::task::block_on(async {
        let addr: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        let listener = yaar::net::TcpListener::bind(addr).expect("failed to bind TcpListener");

        let make_svc =
            make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(hello_world)) });

        let server = Builder::new(compat::HyperListener(listener), Http::new())
            .executor(compat::HyperExecutor)
            .serve(make_svc);

        println!("Listening on http://{}", addr);
        server.await?;
        Ok(())
    })
}

mod compat {
    use std::{
        future::Future,
        io,
        pin::Pin,
        task::{Context, Poll},
    };

    #[derive(Clone)]
    pub struct HyperExecutor;

    impl<F> hyper::rt::Executor<F> for HyperExecutor
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        fn execute(&self, fut: F) {
            yaar::task::spawn(fut);
        }
    }

    pub struct HyperListener(pub yaar::net::TcpListener);

    impl hyper::server::accept::Accept for HyperListener {
        type Conn = HyperStream;
        type Error = io::Error;

        fn poll_accept(
            self: Pin<&mut Self>,
            ctx: &mut Context,
        ) -> Poll<Option<Result<Self::Conn, Self::Error>>> {
            let future = self.0.accept();
            pin_utils::pin_mut!(future);
            match future.poll(ctx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
                Poll::Ready(Ok((stream, _))) => Poll::Ready(Some(Ok(HyperStream(stream)))),
            }
        }
    }

    use tokio::io::{AsyncRead, AsyncWrite};

    pub struct HyperStream(pub yaar::net::TcpStream);

    impl AsyncRead for HyperStream {
        fn poll_read(
            self: Pin<&mut Self>,
            ctx: &mut Context,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            use std::cell::RefCell;
            const TLS_BUF_SIZE: usize = 64 * 1024;
            thread_local!(static TLS_BUF: RefCell<[u8; TLS_BUF_SIZE]> = RefCell::new([0; TLS_BUF_SIZE]));

            TLS_BUF.with(|tls_buf| {
                let mut tls_buf = tls_buf.borrow_mut();
                let read_buf = &mut tls_buf[..buf.remaining()];

                let poll_result = {
                    let future = self.0.read(read_buf);
                    pin_utils::pin_mut!(future);
                    future.poll(ctx)
                };

                match poll_result {
                    Poll::Pending => Poll::Pending,
                    Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                    Poll::Ready(Ok(n)) => Poll::Ready(Ok(buf.put_slice(&mut read_buf[0..n]))),
                }
            })
        }
    }

    impl AsyncWrite for HyperStream {
        fn poll_write(
            self: Pin<&mut Self>,
            ctx: &mut Context,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            let future = self.0.write(buf);
            pin_utils::pin_mut!(future);
            future.poll(ctx)
        }

        fn poll_flush(self: Pin<&mut Self>, _ctx: &mut Context) -> Poll<io::Result<()>> {
            Poll::Ready(self.0.flush())
        }

        fn poll_shutdown(self: Pin<&mut Self>, _ctx: &mut Context) -> Poll<io::Result<()>> {
            Poll::Ready(self.0.shutdown(std::net::Shutdown::Write))
        }
    }
}
