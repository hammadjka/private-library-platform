//! Bounded TCP and HTTP/1 transport for the Axum application.
//!
//! Axum remains responsible for routing and handlers. This module invokes
//! Hyper's connection builder directly because parser-level header limits and
//! connection-lifetime permits must be applied below the router.

use axum::{body::Body, Router};
use hyper::{body::Incoming, server::conn::http1, Request};
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
    time::{self, Sleep},
};
use tower::ServiceExt;

/// Resource limits enforced before a request reaches an Axum handler.
#[derive(Clone, Copy)]
struct TransportLimits {
    /// Maximum number of accepted connections with live Hyper tasks.
    max_connections: usize,
    /// Maximum bytes Hyper may consume for one HTTP request head.
    max_request_head_bytes: usize,
    /// Maximum number of fields allowed in one HTTP request head.
    max_header_fields: usize,
    /// Maximum time allowed to finish reading request headers.
    header_read_timeout: Duration,
    /// Maximum time a connection may go without successful socket I/O.
    inactivity_timeout: Duration,
}

impl Default for TransportLimits {
    /// Returns the resource budget used by the production listener.
    fn default() -> Self {
        Self {
            max_connections: 64,
            max_request_head_bytes: 16 * 1024,
            max_header_fields: 64,
            header_read_timeout: Duration::from_secs(10),
            inactivity_timeout: Duration::from_secs(30),
        }
    }
}

/// Serves the router indefinitely using the production transport limits.
pub(crate) async fn serve(listener: TcpListener, router: Router) -> io::Result<()> {
    serve_with_limits(listener, router, TransportLimits::default()).await
}

/// Runs the permit-before-accept loop with explicit limits.
///
/// Accepting only after a permit is available keeps excess connections in the
/// operating system's bounded listen queue rather than creating unbounded
/// application tasks.
async fn serve_with_limits(
    listener: TcpListener,
    router: Router,
    limits: TransportLimits,
) -> io::Result<()> {
    let connection_slots = Arc::new(Semaphore::new(limits.max_connections));

    loop {
        // Taking a slot before accept applies backpressure at the listening socket instead of
        // accepting an unbounded number of connections into application memory.
        let permit = connection_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| io::Error::other("connection limiter closed"))?;
        let (stream, _) = listener.accept().await?;
        spawn_connection(stream, router.clone(), limits, permit);
    }
}

/// Adapts one accepted Tokio stream to Hyper and spawns its connection task.
///
/// `permit` moves into the task so the connection slot remains occupied for the
/// complete Hyper connection, including keep-alive time and response streaming.
fn spawn_connection(
    stream: TcpStream,
    router: Router,
    limits: TransportLimits,
    permit: OwnedSemaphorePermit,
) {
    // Hyper supplies `Incoming` request bodies. Axum expects its own `Body`
    // wrapper, so map only the body while preserving method, URI, and headers.
    let tower_service = router.map_request(|request: Request<Incoming>| request.map(Body::new));
    // Axum routers implement Tower's Service trait; this adapter presents that
    // service using the interface Hyper's connection driver expects.
    let hyper_service = TowerToHyperService::new(tower_service);

    tokio::spawn(async move {
        let mut http = http1::Builder::new();
        http.max_header_size(limits.max_request_head_bytes)
            .max_headers(limits.max_header_fields)
            .header_read_timeout(limits.header_read_timeout)
            .timer(TokioTimer::new());

        let stream = InactivityTimeout::new(stream, limits.inactivity_timeout);
        // A malformed request, timeout, or peer disconnect ends only this
        // connection. Structured transport diagnostics are a later milestone.
        let _ = http
            .serve_connection(TokioIo::new(stream), hyper_service)
            .await;
        // The permit stays in this task until Hyper finishes serving the connection.
        drop(permit);
    });
}

/// Socket wrapper that fails reads or writes after a period without I/O
/// progress.
///
/// This is an inactivity deadline, not a maximum connection duration. Every
/// successful read or write of at least one byte moves the deadline forward.
struct InactivityTimeout<T> {
    /// Wrapped TCP stream or test I/O object.
    inner: T,
    /// Inactivity interval applied after each successful I/O operation.
    timeout: Duration,
    /// Reusable Tokio timer whose deadline is reset when activity occurs.
    deadline: Pin<Box<Sleep>>,
}

impl<T> InactivityTimeout<T> {
    /// Wraps an I/O object and starts its first inactivity deadline immediately.
    fn new(inner: T, timeout: Duration) -> Self {
        Self {
            inner,
            timeout,
            deadline: Box::pin(time::sleep(timeout)),
        }
    }

    /// Extends the deadline by one full inactivity interval from now.
    fn record_activity(&mut self) {
        self.deadline
            .as_mut()
            .reset(time::Instant::now() + self.timeout);
    }

    /// Polls the timer and translates expiry into an I/O timeout error.
    fn poll_timeout(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.deadline.as_mut().poll(context) {
            Poll::Ready(()) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "connection was inactive",
            ))),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for InactivityTimeout<T> {
    /// Rejects already-expired reads and resets the deadline only when the inner
    /// reader actually produces bytes.
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Poll::Ready(result) = this.poll_timeout(context) {
            return Poll::Ready(result);
        }
        let bytes_before = buffer.filled().len();

        match Pin::new(&mut this.inner).poll_read(context, buffer) {
            Poll::Ready(Ok(())) if buffer.filled().len() > bytes_before => {
                this.record_activity();
                Poll::Ready(Ok(()))
            }
            Poll::Ready(result) => Poll::Ready(result),
            Poll::Pending => this.poll_timeout(context),
        }
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for InactivityTimeout<T> {
    /// Rejects already-expired writes and resets the deadline only when the
    /// inner writer accepts at least one byte.
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let this = self.get_mut();
        if let Poll::Ready(result) = this.poll_timeout(context) {
            return Poll::Ready(result.map(|()| 0));
        }

        match Pin::new(&mut this.inner).poll_write(context, buffer) {
            Poll::Ready(Ok(written)) if written > 0 => {
                this.record_activity();
                Poll::Ready(Ok(written))
            }
            Poll::Ready(result) => Poll::Ready(result),
            Poll::Pending => match this.poll_timeout(context) {
                Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
                Poll::Ready(Ok(())) => unreachable!("timeout never succeeds"),
                Poll::Pending => Poll::Pending,
            },
        }
    }

    /// Applies the inactivity deadline while buffered output is being flushed.
    ///
    /// A successful flush does not reset the timer because it does not report
    /// that any new bytes moved.
    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let this = self.get_mut();
        if let Poll::Ready(result) = this.poll_timeout(context) {
            return Poll::Ready(result);
        }
        match Pin::new(&mut this.inner).poll_flush(context) {
            Poll::Pending => match this.poll_timeout(context) {
                Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
                Poll::Ready(Ok(())) => unreachable!("timeout never succeeds"),
                Poll::Pending => Poll::Pending,
            },
            ready => ready,
        }
    }

    /// Delegates shutdown directly so resource cleanup is not prevented by an
    /// expired inactivity deadline.
    fn poll_shutdown(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn start_test_server(
        limits: TransportLimits,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("read test address");
        let router = crate::server::router(PathBuf::new());
        let task = tokio::spawn(async move {
            let _ = serve_with_limits(listener, router, limits).await;
        });
        (address, task)
    }

    async fn read_response_head(stream: &mut TcpStream) -> Vec<u8> {
        let mut response = Vec::new();
        while !response.windows(4).any(|window| window == b"\r\n\r\n") {
            let mut buffer = [0; 512];
            let read = stream.read(&mut buffer).await.expect("read response head");
            assert!(read > 0, "connection closed before response head completed");
            response.extend_from_slice(&buffer[..read]);
        }
        response
    }

    #[tokio::test]
    async fn closes_connections_that_do_not_finish_headers_before_the_deadline() {
        let limits = TransportLimits {
            header_read_timeout: Duration::from_millis(50),
            inactivity_timeout: Duration::from_secs(2),
            ..TransportLimits::default()
        };
        let (address, server) = start_test_server(limits).await;
        let mut stream = TcpStream::connect(address)
            .await
            .expect("connect to server");
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n")
            .await
            .expect("write incomplete request");

        let mut byte = [0];
        let read = time::timeout(Duration::from_secs(1), stream.read(&mut byte))
            .await
            .expect("header deadline closes promptly")
            .expect("read closed connection");
        assert_eq!(read, 0);
        server.abort();
    }

    #[tokio::test]
    async fn waits_for_connection_capacity_before_accepting_another_connection() {
        let limits = TransportLimits {
            max_connections: 1,
            header_read_timeout: Duration::from_secs(2),
            inactivity_timeout: Duration::from_secs(2),
            ..TransportLimits::default()
        };
        let (address, server) = start_test_server(limits).await;
        let first = TcpStream::connect(address)
            .await
            .expect("open first connection");
        let mut second = TcpStream::connect(address)
            .await
            .expect("open second connection");
        second
            .write_all(b"HEAD / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .expect("write second request");

        let mut byte = [0];
        assert!(
            time::timeout(Duration::from_millis(100), second.read(&mut byte))
                .await
                .is_err(),
            "second connection must wait while the only slot is occupied"
        );

        drop(first);
        let response = time::timeout(Duration::from_secs(1), read_response_head(&mut second))
            .await
            .expect("second connection proceeds after capacity is released");
        assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
        server.abort();
    }

    #[tokio::test]
    async fn closes_a_keep_alive_connection_after_transport_inactivity() {
        let limits = TransportLimits {
            header_read_timeout: Duration::from_secs(2),
            inactivity_timeout: Duration::from_millis(50),
            ..TransportLimits::default()
        };
        let (address, server) = start_test_server(limits).await;
        let mut stream = TcpStream::connect(address)
            .await
            .expect("connect to server");
        stream
            .write_all(b"HEAD / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .expect("write request");
        let response = read_response_head(&mut stream).await;
        assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));

        let mut byte = [0];
        let read = time::timeout(Duration::from_secs(1), stream.read(&mut byte))
            .await
            .expect("inactivity deadline closes promptly")
            .expect("read closed connection");
        assert_eq!(read, 0);
        server.abort();
    }

    #[tokio::test]
    async fn activity_resets_the_deadline_instead_of_limiting_total_duration() {
        let (mut sender, receiver) = tokio::io::duplex(16);
        let mut receiver = InactivityTimeout::new(receiver, Duration::from_millis(50));
        let send_task = tokio::spawn(async move {
            for byte in b"abc" {
                time::sleep(Duration::from_millis(30)).await;
                sender.write_all(&[*byte]).await.expect("write activity");
            }
        });

        let mut received = [0; 3];
        receiver
            .read_exact(&mut received)
            .await
            .expect("periodic activity keeps the transport alive");
        assert_eq!(&received, b"abc");
        send_task.await.expect("activity task finishes");
    }

    #[tokio::test]
    async fn activity_after_the_deadline_does_not_revive_the_transport() {
        let (mut sender, receiver) = tokio::io::duplex(16);
        let mut receiver = InactivityTimeout::new(receiver, Duration::from_millis(30));
        time::sleep(Duration::from_millis(60)).await;
        sender.write_all(b"x").await.expect("write late activity");

        let mut received = [0; 1];
        let error = receiver
            .read_exact(&mut received)
            .await
            .expect_err("expired transport rejects late activity");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
