//! Test-only fixtures (the `testing` feature).
//!
//! [`DropProxy`] is `quictun`'s: it sits between a QUIC client and server on
//! loopback and can stop delivering the server's datagrams, which is how the
//! send-window tests in `phux-relay` and `phux-server` stall a path.

#[cfg(test)]
use std::sync::Arc;

pub use quictun::testing::DropProxy;

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
