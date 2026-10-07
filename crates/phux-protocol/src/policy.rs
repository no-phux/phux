//! Transport-derived peer identity and the QUIC ALPN tokens
//! (`docs/spec/proto.md` §10). None of these types are encoded on the wire;
//! policy logic lives in `phux-server` (ADR-0072).

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

/// Identity of a peer from transport metadata; not every field is available
/// on every transport.
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

/// ALPN protocol id for the QUIC transport (`docs/spec/proto.md` §10). Both
/// ends must offer it exactly, so a non-phux QUIC client never reaches the
/// frame layer.
pub const QUIC_ALPN: &[u8] = b"phux-quic/1";

/// ALPN protocol id for the relay connector leg (ADR-0051). The ALPN alone
/// tells it from consumer connections on the same listener, so it MUST never
/// equal [`QUIC_ALPN`] (ADR-0051 invariant 7).
pub const QUIC_RELAY_ALPN: &[u8] = b"phux-relay/1";

/// ALPN protocol id for workload enrollment on a QUIC listener.
///
/// See `docs/spec/workload-auth.md` §8.2 and ADR-0154. A connection that
/// negotiates it carries one enrollment exchange ([`crate::enroll`]) and no
/// phux frame, so it MUST never equal [`QUIC_ALPN`].
pub const ENROLL_ALPN: &[u8] = b"phux-enroll/1";

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
    /// WebTransport session (HTTP/3 over QUIC), the browser's QUIC path.
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
        assert_eq!(ENROLL_ALPN, b"phux-enroll/1");
        assert_ne!(ENROLL_ALPN, QUIC_ALPN);
        assert_ne!(ENROLL_ALPN, QUIC_RELAY_ALPN);
        assert_ne!(
            QUIC_ALPN, QUIC_RELAY_ALPN,
            "the relay connector leg must never reuse the consumer ALPN \
             (ADR-0051 invariant 7)"
        );
    }
}
