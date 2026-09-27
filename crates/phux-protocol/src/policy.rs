//! Transport-derived peer identity and the QUIC ALPN tokens
//! (`docs/spec/proto.md` §10). None of these types are encoded on the wire;
//! policy logic lives in `phux-server` (ADR-0072).

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

/// Identity of a peer at the transport layer.
///
/// Populated from transport-level metadata: Unix socket credentials,
/// SSH connection info, QUIC certificates, etc. Not all fields are
/// available on all transports.
#[derive(Debug, Clone, Serialize, Deserialize, Hash, Eq, PartialEq)]
pub struct PeerIdentity {
    /// Operating-system user id of the peer process.
    pub uid: u32,
    /// Operating-system process id of the peer process, when available.
    pub pid: Option<u32>,
    /// Filesystem path to the peer executable, when available.
    pub exe_path: Option<String>,
    /// Optional attestation key from an MCP host or other verified consumer.
    pub mcp_host_key: Option<String>,
    /// Transport type that carried this connection.
    pub transport: TransportType,
    /// Network source address, when applicable.
    pub source_addr: Option<IpAddr>,
}

/// ALPN protocol id for the QUIC transport (`docs/spec/proto.md` §10).
///
/// QUIC mandates ALPN, so both the server listener and the client dialer must
/// offer this exact token or the TLS handshake fails — which also keeps a stray
/// non-phux QUIC client (or a protocol-version mismatch) from ever reaching the
/// frame layer. Defined here, in the wire crate, so the two ends cannot drift.
pub const QUIC_ALPN: &[u8] = b"phux-quic/1";

/// ALPN protocol id for the relay connector leg (ADR-0051, Decision 2).
///
/// The dial-out connector's tunnel to a relay and ordinary consumer
/// connections can terminate at the same QUIC listener; the negotiated ALPN
/// — never the byte stream — is what tells the two legs apart. This token
/// must therefore never equal [`QUIC_ALPN`] (ADR-0051 invariant 7). Defined
/// here, in the wire crate, so connector and relay cannot drift.
pub const QUIC_RELAY_ALPN: &[u8] = b"phux-relay/1";

/// Transport classification for peer identity.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Hash, Eq, PartialEq)]
pub enum TransportType {
    /// Unix domain socket (local machine).
    UnixSocket,
    /// Tunnelled over an existing SSH connection.
    SshTunnel,
    /// QUIC direct connection.
    Quic,
    /// WebSocket upgrade (browser or proxy).
    WebSocket,
    /// WebTransport session (HTTP/3 over QUIC) — the browser's door to
    /// QUIC-class transport, since browsers cannot open raw QUIC streams.
    WebTransport,
    /// Loopback / same-process.
    Localhost,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpn_tokens_are_pinned_and_distinct() {
        // Wire constants: changing either byte string is a breaking change
        // for every deployed peer, so an accidental edit must fail loudly.
        assert_eq!(QUIC_ALPN, b"phux-quic/1");
        assert_eq!(QUIC_RELAY_ALPN, b"phux-relay/1");
        assert_ne!(
            QUIC_ALPN, QUIC_RELAY_ALPN,
            "the relay connector leg must never reuse the consumer ALPN \
             (ADR-0051 invariant 7)"
        );
    }
}
