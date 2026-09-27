//! Test-only fixtures (the `testing` feature).
//!
//! [`DropProxy`] sits between a QUIC client and server on loopback and can
//! stop delivering the server's datagrams, so no ack returns and the server's
//! congestion window freezes: the send-window tests here, in `phux-relay`, and
//! in `phux-server` use it.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

/// A loopback UDP forwarder between one client and one upstream server.
///
/// Datagrams from the upstream address go to the most recent client; every
/// other datagram goes upstream. Aborts its task on drop.
#[derive(Debug)]
pub struct DropProxy {
    addr: SocketAddr,
    drop_downstream: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl DropProxy {
    /// Bind a proxy on an ephemeral loopback port forwarding to `upstream`.
    ///
    /// # Errors
    ///
    /// Returns the bind error when no loopback UDP port is available.
    pub async fn start(upstream: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind("127.0.0.1:0").await?;
        let addr = socket.local_addr()?;
        let drop_downstream = Arc::new(AtomicBool::new(false));
        let task = tokio::spawn(forward(socket, upstream, Arc::clone(&drop_downstream)));
        Ok(Self {
            addr,
            drop_downstream,
            task,
        })
    }

    /// The address clients dial instead of the upstream server.
    #[must_use]
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Silently drop every upstream-to-client datagram from now on.
    pub fn blackhole_downstream(&self) {
        self.drop_downstream.store(true, Ordering::SeqCst);
    }
}

impl Drop for DropProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The forwarding loop. Ends on the first socket error.
async fn forward(socket: UdpSocket, upstream: SocketAddr, drop_downstream: Arc<AtomicBool>) {
    let mut buf = vec![0_u8; 64 * 1024];
    let mut client = None;
    while let Ok((len, from)) = socket.recv_from(&mut buf).await {
        let target = if from == upstream {
            if drop_downstream.load(Ordering::SeqCst) {
                continue;
            }
            match client {
                Some(client) => client,
                None => continue,
            }
        } else {
            client = Some(from);
            upstream
        };
        if socket.send_to(&buf[..len], target).await.is_err() {
            return;
        }
    }
}

/// A TLS 1.3 server config over a fresh self-signed pair in `dir`, offering
/// `alpn` when given. Shared by this crate's QUIC and WebSocket tests.
#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
pub(crate) fn server_tls(dir: &std::path::Path, alpn: Option<&[u8]>) -> rustls::ServerConfig {
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
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
    if let Some(alpn) = alpn {
        tls.alpn_protocols = vec![alpn.to_vec()];
    }
    tls
}

/// A loopback quinn server endpoint over [`server_tls`].
#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
pub(crate) fn quic_server(dir: &std::path::Path, alpn: &[u8]) -> quinn::Endpoint {
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(server_tls(dir, Some(alpn)))
        .expect("quic crypto");
    let config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    quinn::Endpoint::server(config, "127.0.0.1:0".parse().expect("addr")).expect("bind server")
}
