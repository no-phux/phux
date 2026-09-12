//! Congestion-tracked QUIC send windows: TCP's `NOTSENT_LOWAT` rule for quinn.
//!
//! quinn's default send window is 10 MB, bounded in practice by the peer's
//! 1.25 MB stream credit. On a link slower than the output (cmatrix over a
//! thin Wi-Fi or DERP path) a writer kept accepting until that credit was
//! spent, so the backlog — and the lag in front of every keystroke echo —
//! grew to seconds before the writer ever blocked. Backpressure never reached
//! whatever was producing the output, so nothing upstream could react to it.
//!
//! Holding the window to the congestion window plus [`UNSENT_SLACK`] keeps
//! what is unsent small, so a slow link queues about one round trip of output
//! instead of megabytes, and a writer that finds the window full blocks. The
//! window is re-read before every partial write so it follows the path as the
//! congestion controller learns it.
//!
//! This is the one implementation every phux QUIC writer whose output can
//! outrun its path shares: the server's raw-QUIC and WebTransport writers
//! (wtransport rides quinn) and `phux-relay`'s consumer-facing leg.

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use tokio::io::AsyncWrite;

/// Output quinn may hold beyond what the congestion window lets it send.
///
/// The slack keeps the next write ready when an ack opens room, so the path
/// stays window-limited and the congestion window keeps growing on a fast
/// link; anything more is backlog a slow link turns into lag.
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
/// quinn's send window belongs to the connection, not to a stream, so this
/// is built once per connection and cloned for every stream written on it.
/// Clones share the value last handed to quinn: a connection that carries
/// many streams (a relay tunnel bridging several consumers) has one window,
/// and a per-writer cache would let one writer skip an update because it
/// remembers a value another writer has since replaced.
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
    /// quinn's setter wakes the connection driver, so it is only called when
    /// the value changes. The congestion window is read, compared and applied
    /// under one lock: read outside it, two clones could race and the one
    /// holding the older congestion window could overwrite the newer value.
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
/// Wraps anything that writes into a quinn connection — `quinn::SendStream`,
/// or a stream layered on one such as wtransport's — and is itself an
/// [`AsyncWrite`], so `write_all` and `tokio::io::copy` get the bound without
/// knowing about it.
///
/// Per write, not per buffer: pinning the window once for a whole
/// `write_all` would freeze it for the length of a bootstrap batch that runs
/// to megabytes. Once the congestion window grew past the pinned value quinn
/// would be starved of unsent data, count the path as app-limited and stop
/// growing the window — a fresh connection would crawl through its first
/// screen at one pinned window per round trip instead of ramping up in slow
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
        // Even a collapsed path keeps the slack, so the writer always has
        // somewhere to put its next batch.
        assert_eq!(send_window_for(0), UNSENT_SLACK);
        // In range, only the slack sits beyond what the path may send.
        assert_eq!(send_window_for(14_720), 14_720 + UNSENT_SLACK);
        assert_eq!(send_window_for(100_000), 100_000 + UNSENT_SLACK);
        // A huge window is capped at quinn's own default, without overflow.
        assert_eq!(send_window_for(MAX_SEND_WINDOW), MAX_SEND_WINDOW);
        assert_eq!(send_window_for(u64::MAX), MAX_SEND_WINDOW);
    }

    /// A quinn server endpoint on an ephemeral loopback port with a fresh
    /// self-signed certificate (the tempdir holds the PEM files).
    fn server_endpoint() -> (tempfile::TempDir, quinn::Endpoint) {
        let dir = tempfile::tempdir().expect("tempdir");
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        crate::cert::ensure_self_signed(&cert, &key).expect("provision cert");
        let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("tls13")
        .with_no_client_auth()
        .with_single_cert(
            crate::cert::load_certs(&cert).expect("certs"),
            crate::cert::load_key(&key).expect("key"),
        )
        .expect("server tls");
        tls.alpn_protocols = vec![ALPN.to_vec()];
        let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls).expect("quic crypto");
        let config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
        let endpoint = quinn::Endpoint::server(config, "127.0.0.1:0".parse().expect("addr"))
            .expect("bind server");
        (dir, endpoint)
    }

    /// A quinn client endpoint that trusts the throwaway certificate.
    fn client_endpoint() -> quinn::Endpoint {
        let crypto = client_config(&CertTrust::SkipVerify, Some(ALPN)).expect("client tls");
        let mut endpoint =
            quinn::Endpoint::client("127.0.0.1:0".parse().expect("addr")).expect("bind client");
        endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto).expect("quic crypto"),
        )));
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
}
