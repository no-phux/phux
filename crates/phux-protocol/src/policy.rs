//! Policy and extensibility types for server-side policy engines, audit
//! logging, and consumer identity tracking.
//!
//! These types are wire-friendly and serializable so they can be embedded
//! in audit events, capability tokens, and structured logs. They carry no
//! policy logic — that lives in the consumer of these types (typically
//! `phux-server` or a downstream policy crate).

use std::collections::HashMap;
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

/// A consumer identifier.
#[derive(Debug, Clone, Serialize, Deserialize, Hash, Eq, PartialEq)]
pub struct ConsumerId(pub String);

/// A capability token grants fine-grained access to operations.
///
/// Capability tokens are exchanged at HELLO time and scoped for the
/// lifetime of the connection. The server may attenuate the requested
/// capabilities based on its own policy.
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

/// Decision from a policy authorization check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Decision {
    /// The operation is permitted.
    Allow,
    /// The operation is denied with a human-readable reason.
    Deny {
        /// Human-readable explanation for the denial.
        reason: String,
    },
    /// The operation is permitted but flagged with a threat score.
    /// The policy engine may also trigger an alert via the audit sink.
    Alert {
        /// Heuristic threat score in `[0.0, 1.0]`; higher is riskier.
        threat_score: f64,
    },
    /// The operation requires an additional challenge before proceeding.
    Challenge {
        /// The challenge mechanism the consumer must satisfy.
        mechanism: ChallengeType,
    },
}

/// Challenge mechanism for `Decision::Challenge`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChallengeType {
    /// Time-based one-time password.
    Totp,
    /// Force re-authentication at the transport layer.
    ReAuthenticate,
}

/// Structured audit event for external observers.
///
/// Audit events are emitted at every security-relevant decision point.
/// They are intentionally self-contained so they can be shipped to an
/// external sink without server-side state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Event timestamp (UTC).
    pub ts: chrono::DateTime<chrono::Utc>,
    /// Server-unique event identifier.
    pub event_id: String,
    /// The consumer that initiated the action.
    pub consumer: ConsumerId,
    /// Transport-level identity of the peer.
    pub peer: PeerIdentity,
    /// The action that was attempted.
    pub action: AuditAction,
    /// The resource the action targeted.
    pub target: AuditTarget,
    /// The policy decision that was applied.
    pub decision: Decision,
    /// Microseconds spent in the policy check.
    pub latency_us: u64,
    /// Opaque metadata for policy-engine-specific extensions.
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Actions that may be audited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuditAction {
    /// HELLO handshake.
    Hello,
    /// L1 terminal operation.
    TerminalOp(TerminalOp),
    /// Group operation.
    GroupOp(GroupOp),
    /// L3 metadata operation.
    MetadataOp(MetadataOp),
    /// Satellite routing operation (federation).
    SatelliteRoute {
        /// Originating satellite host.
        from: String,
        /// Destination satellite host.
        to: String,
    },
}

/// Terminal-level operations that may be audited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TerminalOp {
    /// Snapshot of terminal state.
    Snapshot {
        /// Whether scrollback was included in the snapshot.
        scrollback: bool,
    },
    /// Key input.
    InputKey,
    /// Mouse input.
    InputMouse,
    /// Paste input.
    InputPaste,
    /// Spawn a new terminal.
    Spawn,
    /// Kill a terminal.
    Kill,
    /// Resize a terminal.
    Resize,
    /// Attach to a terminal.
    Attach,
    /// Detach from a terminal.
    Detach,
}

/// Group-level operations that may be audited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GroupOp {
    /// Create a new group.
    Create,
    /// Kill a group and its terminals.
    Kill,
    /// Add a terminal to a group.
    AddTerminal,
    /// Remove a terminal from a group.
    RemoveTerminal,
    /// Rename a group.
    Rename,
    /// List groups.
    List,
}

/// Metadata-level operations that may be audited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MetadataOp {
    /// Read a metadata value.
    Get,
    /// Write a metadata value of the given byte size.
    Set {
        /// Size in bytes of the value being written.
        size: usize,
    },
    /// Delete a metadata value.
    Delete,
    /// List metadata keys.
    List,
}

/// Scope for metadata operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MetadataScope {
    /// Global scope.
    Global,
    /// Terminal-scoped.
    Terminal {
        /// The terminal the metadata is scoped to.
        id: TerminalId,
    },
    /// Group-scoped.
    Group {
        /// The group the metadata is scoped to.
        id: GroupId,
    },
}

/// Target resource of an audited action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditTarget {
    /// Terminal id, when applicable.
    pub terminal_id: Option<TerminalId>,
    /// Group id, when applicable.
    pub group_id: Option<GroupId>,
    /// Satellite host, when applicable.
    pub satellite: Option<String>,
}

/// Classification of a consumer for provenance tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsumerClass {
    /// Human using the reference TUI.
    HumanTui,
    /// Agent via MCP adapter.
    AgentMcp,
    /// Agent via SDK or direct API.
    AgentSdk,
    /// Automated system (CI, cron, etc.).
    Automation,
    /// Unclassified.
    Unknown,
}

/// Provenance tag attached to an input frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputTag {
    /// The consumer that produced the input.
    pub consumer: ConsumerId,
    /// Classification of the consumer.
    pub class: ConsumerClass,
    /// Chain of intermediaries (e.g. `["mcp", "claude", "phux-mcp"]`).
    pub chain: Vec<String>,
    /// Timestamp of the input.
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// A raw input frame with provenance metadata attached.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaggedInput {
    /// The terminal this input targets.
    pub terminal_id: TerminalId,
    /// The raw input payload.
    pub payload: Vec<u8>,
    /// Provenance tag.
    pub tag: InputTag,
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
