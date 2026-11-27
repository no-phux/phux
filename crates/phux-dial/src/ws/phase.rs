//! The dial observer's closed, payload-free vocabulary.

/// Payload-free establishment boundaries, observed before each operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsPhase {
    /// Resolve the target's host name.
    Resolve,
    /// Connect the TCP socket.
    TcpConnect,
    /// Prepare TLS trust and the client identity.
    TlsConfig,
    /// Complete the TLS handshake.
    TlsHandshake,
    /// Build and exchange the HTTP WebSocket upgrade.
    Upgrade,
    /// The peer accepted the WebSocket upgrade.
    Upgraded,
}
