//! Congestion-tracked QUIC send windows: TCP's `NOTSENT_LOWAT` rule for quinn.
//!
//! quinn's default send window lets a writer on a slow link queue megabytes
//! (seconds of lag) before it blocks. Holding the window to the congestion
//! window plus [`UNSENT_SLACK`], re-read before every partial write, queues
//! about one round trip instead and pushes backpressure to the producer.
//! Shared by the server's raw-QUIC and WebTransport writers and the relay.

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use tokio::io::AsyncWrite;

/// Output quinn may hold beyond what the congestion window lets it send: enough
/// to keep the path window-limited so the congestion window keeps growing.
pub const UNSENT_SLACK: u64 = 16 * 1024;

/// Ceiling for the tracked send window: quinn's own default.
pub const MAX_SEND_WINDOW: u64 = 10 * 1024 * 1024;

/// The send window for a path whose congestion window is `cwnd` bytes.
#[must_use]
pub const fn send_window_for(cwnd: u64) -> u64 {
    let window = cwnd.saturating_add(UNSENT_SLACK);
    if window < MAX_SEND_WINDOW {
        window
    } else {
        MAX_SEND_WINDOW
    }
}

/// One connection's congestion-tracked send window.
///
/// quinn's send window is per connection, so this is built once per
/// connection and cloned per stream; clones share the last applied value so
/// one writer never skips an update another writer invalidated.
#[derive(Debug, Clone)]
pub struct SendWindow {
    conn: quinn::Connection,
    /// The window last handed to quinn; `0` before the first update.
    applied: Arc<Mutex<u64>>,
}

impl SendWindow {
    /// Track `conn`'s send window. Nothing changes until the first
    /// [`track`](Self::track).
    #[must_use]
    pub fn new(conn: quinn::Connection) -> Self {
        Self {
            conn,
            applied: Arc::new(Mutex::new(0)),
        }
    }

    /// Hold the connection's send window to its congestion window plus
    /// [`UNSENT_SLACK`].
    ///
    /// Only calls quinn's setter (which wakes the driver) on change. Read,
    /// compare and apply happen under one lock so racing clones cannot
    /// overwrite a newer value with an older one.
    pub fn track(&self) {
        let mut applied = self.applied.lock().unwrap_or_else(PoisonError::into_inner);
        let window = send_window_for(self.conn.stats().path.cwnd);
        if *applied != window {
            self.conn.set_send_window(window);
            *applied = window;
        }
    }

    /// The connection whose window this tracks.
    #[must_use]
    pub const fn connection(&self) -> &quinn::Connection {
        &self.conn
    }
}

/// A QUIC send stream that re-tracks its connection's [`SendWindow`] before
/// every partial write.
///
/// Per write, not per buffer: pinning the window once for a multi-megabyte
/// `write_all` would starve quinn, mark the path app-limited, and stall slow
/// start.
#[derive(Debug)]
pub struct TrackedSend<W> {
    inner: W,
    window: SendWindow,
}

impl<W> TrackedSend<W> {
    /// Wrap `inner`, which must write into `window`'s connection.
    #[must_use]
    pub const fn new(inner: W, window: SendWindow) -> Self {
        Self { inner, window }
    }

    /// The wrapped stream.
    #[must_use]
    pub const fn get_ref(&self) -> &W {
        &self.inner
    }

    /// The wrapped stream, mutably — for operations outside `AsyncWrite`,
    /// such as quinn's synchronous `finish`.
    pub const fn get_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// The connection window this stream tracks.
    #[must_use]
    pub const fn window(&self) -> &SendWindow {
        &self.window
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for TrackedSend<W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.window.track();
        Pin::new(&mut this.inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use std::net::SocketAddr;
    use std::time::Duration;

    use tokio::io::AsyncWriteExt;

    use super::*;
    use crate::testing::DropProxy;
    use crate::tls::{CertTrust, client_config};

    const ALPN: &[u8] = b"phux-window-test";

    #[test]
    fn send_window_is_the_congestion_window_plus_unsent_slack() {
        assert_eq!(send_window_for(0), UNSENT_SLACK);
        assert_eq!(send_window_for(100_000), 100_000 + UNSENT_SLACK);
        // Capped at quinn's own default, without overflow.
        assert_eq!(send_window_for(MAX_SEND_WINDOW), MAX_SEND_WINDOW);
        assert_eq!(send_window_for(u64::MAX), MAX_SEND_WINDOW);
    }

    fn server_endpoint() -> (tempfile::TempDir, quinn::Endpoint) {
        let dir = tempfile::tempdir().expect("tempdir");
        let endpoint = crate::testing::quic_server(dir.path(), ALPN);
        (dir, endpoint)
    }

    /// A quinn client endpoint that trusts the throwaway certificate.
    fn client_endpoint() -> quinn::Endpoint {
        client_endpoint_with_transport(quinn::TransportConfig::default())
    }

    fn client_endpoint_with_transport(transport: quinn::TransportConfig) -> quinn::Endpoint {
        let crypto = client_config(&CertTrust::SkipVerify, Some(ALPN)).expect("client tls");
        let mut endpoint =
            quinn::Endpoint::client("127.0.0.1:0".parse().expect("addr")).expect("bind client");
        let mut config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto).expect("quic crypto"),
        ));
        config.transport_config(Arc::new(transport));
        endpoint.set_default_client_config(config);
        endpoint
    }

    /// Connect a client through a [`DropProxy`] and return the server side of
    /// the connection, the proxy, and the endpoints that keep it alive.
    async fn connect_through_proxy() -> (
        quinn::Connection,
        DropProxy,
        (
            tempfile::TempDir,
            quinn::Endpoint,
            quinn::Endpoint,
            quinn::Connection,
        ),
    ) {
        let (dir, server) = server_endpoint();
        let server_addr: SocketAddr = server.local_addr().expect("server addr");
        let proxy = DropProxy::start(server_addr).await.expect("proxy");
        let client = client_endpoint();
        let dialed = async {
            client
                .connect(proxy.addr(), "localhost")
                .expect("connect")
                .await
                .expect("client handshake")
        };
        let accepted = async {
            server
                .accept()
                .await
                .expect("incoming")
                .await
                .expect("server handshake")
        };
        let (client_conn, server_conn) = tokio::join!(dialed, accepted);
        (server_conn, proxy, (dir, server, client, client_conn))
    }

    /// Bytes a writer accepts once the path stops acknowledging anything,
    /// before a write stays blocked for a while.
    async fn accepted_before_stall<W: AsyncWrite + Unpin>(writer: &mut W) -> usize {
        let chunk = vec![0x5a_u8; 64 * 1024];
        let mut accepted = 0;
        while accepted < 8 * 1024 * 1024 {
            match tokio::time::timeout(Duration::from_millis(300), writer.write(&chunk)).await {
                Ok(Ok(written)) => accepted += written,
                Ok(Err(err)) => panic!("write failed: {err}"),
                Err(_) => break,
            }
        }
        accepted
    }

    /// With acks cut off, an untracked stream buffers up to the peer's 1.25 MB
    /// stream credit; a tracked one stops at about one congestion window plus
    /// the slack. The untracked half keeps the test honest: if quinn's
    /// defaults ever stop buffering, the tracked assertion alone would pass
    /// without proving anything.
    #[tokio::test]
    async fn tracked_writer_blocks_near_the_congestion_window_when_the_path_stalls() {
        let (server_conn, proxy, _keep) = connect_through_proxy().await;
        let mut untracked = server_conn.open_uni().await.expect("untracked stream");
        proxy.blackhole_downstream();
        let untracked_accepted = accepted_before_stall(&mut untracked).await;
        assert!(
            untracked_accepted > 1024 * 1024,
            "quinn's default window buffers megabytes on a stalled path (got {untracked_accepted})"
        );

        let (server_conn, proxy, _keep) = connect_through_proxy().await;
        let send = server_conn.open_uni().await.expect("tracked stream");
        let mut tracked = TrackedSend::new(send, SendWindow::new(server_conn.clone()));
        proxy.blackhole_downstream();
        let tracked_accepted = accepted_before_stall(&mut tracked).await;
        let cwnd = server_conn.stats().path.cwnd;
        assert!(
            tracked_accepted as u64 <= send_window_for(cwnd).max(64 * 1024),
            "tracked writer accepted {tracked_accepted} bytes against a {cwnd}-byte congestion window"
        );
    }

    /// Clones share one cached value, so a writer that last set the window
    /// still updates it after another writer on the same connection changed
    /// it underneath.
    #[tokio::test]
    async fn clones_share_the_applied_window() {
        let (server_conn, _proxy, _keep) = connect_through_proxy().await;
        let first = SendWindow::new(server_conn.clone());
        let second = first.clone();
        first.track();
        let expected = send_window_for(server_conn.stats().path.cwnd);
        assert_eq!(*second.applied.lock().expect("lock"), expected);
        // Simulate another writer having handed quinn a different value.
        *second.applied.lock().expect("lock") = 1;
        first.track();
        assert_eq!(*first.applied.lock().expect("lock"), expected);
    }

    /// Exhaust a known peer stream window, keep the next write pending, and
    /// prove another stream progresses through the same production window.
    /// Reading the stalled stream must release that exact pending write.
    #[tokio::test]
    async fn exhausted_stream_credit_preserves_other_stream_progress() {
        tokio::time::timeout(Duration::from_secs(5), async {
            const CREDIT: u32 = 32 * 1024;
            let (_dir, server) = server_endpoint();
            let mut transport = quinn::TransportConfig::default();
            transport.stream_receive_window(CREDIT.into());
            transport.receive_window((16 * CREDIT).into());
            let client = client_endpoint_with_transport(transport);
            let dialed = client
                .connect(server.local_addr().expect("server addr"), "localhost")
                .expect("connect");
            let accepted = async { server.accept().await.expect("incoming").await };
            let (client_conn, server_conn) = tokio::join!(dialed, accepted);
            let client_conn = client_conn.expect("client handshake");
            let server_conn = server_conn.expect("server handshake");
            let window = SendWindow::new(server_conn.clone());
            let stream = server_conn.open_uni().await.expect("flood stream");
            let mut flood = TrackedSend::new(stream, window.clone());
            let payload = vec![0x5a; CREDIT as usize];
            flood.write_all(&payload).await.expect("fill peer credit");
            let mut flood_recv = client_conn.accept_uni().await.expect("accept flood");
            let blocked = flood.write_all(b"x");
            tokio::pin!(blocked);
            assert!(
                tokio::time::timeout(Duration::from_millis(100), &mut blocked)
                    .await
                    .is_err(),
                "the unread stream must consume all of its advertised credit"
            );

            let quiet_stream = server_conn.open_uni().await.expect("quiet stream");
            let mut quiet = TrackedSend::new(quiet_stream, window);
            quiet.write_all(b"quiet").await.expect("quiet write");
            let mut quiet_recv = client_conn.accept_uni().await.expect("accept quiet");
            let mut message = [0; 5];
            quiet_recv
                .read_exact(&mut message)
                .await
                .expect("quiet read");
            assert_eq!(&message, b"quiet");
            assert!(
                tokio::time::timeout(Duration::from_millis(100), &mut blocked)
                    .await
                    .is_err(),
                "quiet progress must not replenish the stalled stream's credit"
            );

            let mut drained = vec![0; CREDIT as usize];
            flood_recv
                .read_exact(&mut drained)
                .await
                .expect("drain flood");
            assert_eq!(drained, payload);
            blocked.await.expect("reading replenishes stream credit");
            let mut tail = [0];
            flood_recv
                .read_exact(&mut tail)
                .await
                .expect("read final byte");
            assert_eq!(&tail, b"x");
            server_conn.close(0u32.into(), b"test complete");
            client_conn.close(0u32.into(), b"test complete");
        })
        .await
        .expect("stream-credit isolation test completed before deadline");
    }
}
