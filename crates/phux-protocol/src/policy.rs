//! Transport-derived peer identity and the capability vocabulary the
//! server's authorization seam speaks.
//!
//! Scope is deliberately narrow (ADR-0072): this module carries only the
//! vocabulary that live code produces or consumes — the ALPN tokens the
//! QUIC listeners and dialers pin (`docs/spec/proto.md` §10), the
//! [`PeerIdentity`] every transport stamps on an accepted connection, and
//! the [`Capability`] set `phux-server`'s HELLO authorization seam passes
//! and returns. It carries no policy logic; that lives in `phux-server`.
//!
//! None of these types are encoded on the wire. Authorization vocabulary
//! that no encoder, decoder, or call site referenced — an audit-event
//! taxonomy, an input-provenance tag, and a per-operation decision enum —
//! was removed rather than frozen into a published crate; see ADR-0072 and
//! the feature that will reintroduce whatever it actually needs (phux-pjc5).

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::caps::Layer;
use crate::ids::{GroupId, TerminalId};

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

/// A capability grants fine-grained access to operations.
///
/// A capability set is scoped to the lifetime of one connection: the server
/// derives the requested set at HELLO and its policy engine returns the
/// granted set, which may be an attenuation of what was requested. **Not a
/// wire type** — HELLO carries no capability request today, so the set is
/// server-internal until phux-pjc5 mints connection-bound scopes from a
/// paired workload credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    /// The protocol layer this capability applies to.
    pub layer: Layer,
    /// Whitelist of operation names (e.g. `"snapshot"`, `"input"`).
    /// An empty vec means no operations are granted.
    pub ops: Vec<String>,
    /// Optional restriction to specific terminals. `None` means all
    /// terminals in scope.
    pub terminals: Option<Vec<TerminalId>>,
    /// Optional restriction to specific groups. `None` means all
    /// groups in scope.
    pub groups: Option<Vec<GroupId>>,
    /// Optional expiry time. `None` means the capability is valid for
    /// the lifetime of the connection.
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
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
