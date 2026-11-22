//! Test-only fixtures (the `testing` feature).
//!
//! [`DropProxy`] is `quictun`'s: it sits between a QUIC client and server on
//! loopback and can stop delivering the server's datagrams, which is how the
//! send-window tests in the relay contract and `phux-server` stall a path.

pub use quictun::testing::DropProxy;

/// The loopback TLS and QUIC servers this crate's QUIC and WebSocket tests
/// dial.
#[cfg(test)]
pub(crate) use quictun::testing::{quic_server, server_tls};
