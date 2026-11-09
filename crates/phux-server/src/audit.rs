//! `phux.audit/v1`: an append-only, hash-chained, ed25519-signed record of
//! who did what on this host (`docs/audit.md`).
//!
//! The server appends one JSON line per decision to
//! `$STATE/audit/audit.jsonl`. Each record's `hash` is the SHA-256 of its
//! canonical form (compact JSON, keys sorted at every level, `hash` and
//! `sig` omitted), `prev` is the previous record's `hash`, and `sig` is an
//! ed25519 signature over the 32 hash bytes. The file rotates at 64 MiB to
//! `audit.jsonl.1` .. `.4`; the chain runs on across files.
//!
//! Writing never blocks or fails a command: an error logs a warning and
//! counts in `audit.write_failures` (`GET_PERF`, `phux doctor`). The reader
//! half ([`read_records`], [`verify_dir`]) needs no server.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::auth::ConnectionIdentity;

mod reader;
mod sink;
mod writer;

pub use reader::{
    Unreadable, VerifyFailure, VerifyReport, log_files, read_records, tail, verify_dir,
};
pub(crate) use sink::registry_loaded;
pub use sink::{Recording, default_dir, emit, enabled, init, record, refused, shutdown};
#[cfg(test)]
pub(crate) use sink::{capture_for_test, release_test_capture};
pub use writer::{AuditLog, Entry};

/// The `v` every record carries.
pub const SCHEMA_VERSION: u32 = 1;
/// The live file's name inside the audit directory.
pub const LOG_FILE: &str = "audit.jsonl";
/// The private signing key (PKCS#8 DER, owner-only).
pub const KEY_FILE: &str = "audit-key";
/// The public key, lowercase hex of the 32 raw bytes.
pub const PUBLIC_KEY_FILE: &str = "audit-key.pub";
/// The live file rotates once it would pass this size.
pub const ROTATE_BYTES: u64 = 64 * 1024 * 1024;
/// Rotated files kept: `audit.jsonl.1` (newest) .. `.4` (oldest).
pub const KEEP_ROTATED: u32 = 4;
/// `prev` of the first record ever written.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// What a record is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// A connection was admitted.
    #[serde(rename = "connection.open")]
    ConnectionOpen,
    /// An admitted connection ended.
    #[serde(rename = "connection.close")]
    ConnectionClose,
    /// A remote peer failed admission (bad certificate or token).
    #[serde(rename = "connection.refused")]
    ConnectionRefused,
    /// A command or state-changing frame was authorized.
    #[serde(rename = "command")]
    Command,
    /// A command or frame was denied.
    #[serde(rename = "command.refused")]
    CommandRefused,
    /// A command was held for approval.
    #[serde(rename = "approval.held")]
    ApprovalHeld,
    /// A held command was approved, denied, expired, or withdrawn.
    #[serde(rename = "approval.decided")]
    ApprovalDecided,
    /// A workload credential appeared in the registry.
    #[serde(rename = "credential.added")]
    CredentialAdded,
    /// A workload credential left the registry or was revoked.
    #[serde(rename = "credential.removed")]
    CredentialRemoved,
    /// A `GET_FILE` chunk was authorized.
    #[serde(rename = "file.get")]
    FileGet,
    /// A `PUT_FILE` chunk was authorized.
    #[serde(rename = "file.put")]
    FilePut,
    /// A Terminal's input lease was taken or given back.
    #[serde(rename = "input.lease")]
    InputLease,
}

/// How the decision came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// Admitted or performed.
    Allowed,
    /// Refused by policy or admission.
    Denied,
    /// Waiting for an approval.
    Held,
    /// Approved by a decider.
    Approved,
    /// Denied by a decider, expired, or withdrawn.
    Rejected,
}

/// The transport an actor came in on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// The owner Unix socket (or the server itself).
    Local,
    /// QUIC or WebTransport.
    Quic,
    /// WebSocket.
    Wss,
    /// A same-uid `phux stdio-bridge` over ssh.
    Ssh,
}

/// Who acted. Every field but `transport` is `null` when unknown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditActor {
    /// The credential id (`sha256:...` for a workload key).
    pub credential: Option<String>,
    /// The credential's principal (`human`, `automation`, ...).
    pub principal: Option<String>,
    /// The person or agent the credential belongs to.
    pub subject: Option<String>,
    /// The credential that minted this one, for a delegated key.
    pub on_behalf_of: Option<String>,
    /// How the actor reached the server.
    pub transport: Transport,
}

impl Transport {
    /// The audit transport for a peer's transport.
    #[must_use]
    pub const fn of(transport: phux_protocol::policy::TransportType) -> Self {
        use phux_protocol::policy::TransportType;
        match transport {
            TransportType::UnixSocket | TransportType::Localhost => Self::Local,
            TransportType::SshTunnel => Self::Ssh,
            TransportType::Quic | TransportType::WebTransport => Self::Quic,
            TransportType::WebSocket => Self::Wss,
        }
    }
}

impl AuditActor {
    /// An unauthenticated peer on `transport` (a refused connection).
    #[must_use]
    pub const fn anonymous(transport: Transport) -> Self {
        Self {
            credential: None,
            principal: None,
            subject: None,
            on_behalf_of: None,
            transport,
        }
    }

    /// The server itself (an expiry, a registry change).
    #[must_use]
    pub const fn server() -> Self {
        Self::anonymous(Transport::Local)
    }
}

/// The actor a connection's identity names. The one place the audit actor
/// is built, so a new identity field lands here once.
#[must_use]
pub fn audit_actor(identity: &ConnectionIdentity) -> AuditActor {
    let transport = if identity.ssh_origin.is_some() {
        Transport::Ssh
    } else {
        Transport::of(identity.peer.transport)
    };
    let credential = identity.credential.as_ref();
    AuditActor {
        credential: credential.map(|credential| credential.id.clone()),
        principal: credential.map(|credential| credential.principal.clone()),
        // Filled from the credential once it carries a subject (C1) and a
        // delegating credential (C3).
        subject: None,
        on_behalf_of: None,
        transport,
    }
}

/// One line of the log, in canonical field order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// [`SCHEMA_VERSION`].
    pub v: u32,
    /// Position in the chain, from 0, contiguous across rotation.
    pub seq: u64,
    /// RFC 3339 UTC with milliseconds.
    pub ts: String,
    /// The host that wrote it.
    pub host: String,
    /// What it is about.
    pub kind: Kind,
    /// Who acted.
    pub actor: AuditActor,
    /// The command or frame name (`KILL_RESOURCE`), or the event (`open`).
    pub action: String,
    /// What it acted on (`terminal:3`, a path, a credential id), if anything.
    pub target: Option<String>,
    /// How it came out.
    pub outcome: Outcome,
    /// Kind-specific facts; never terminal bytes, file contents, or tokens.
    pub detail: Value,
    /// The previous record's `hash`, or [`GENESIS`].
    pub prev: String,
    /// Hex SHA-256 of the canonical form.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hash: String,
    /// Base64 ed25519 signature over the 32 `hash` bytes.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sig: String,
}

impl AuditRecord {
    /// The bytes `hash` covers: the record without `hash` and `sig` as
    /// compact JSON, object keys sorted at every level, strings escaped as
    /// `serde_json` writes them.
    #[must_use]
    pub fn canonical(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.hash.clear();
        unsigned.sig.clear();
        let mut out = String::new();
        if let Ok(value) = serde_json::to_value(&unsigned) {
            write_sorted(&value, &mut out);
        }
        out.into_bytes()
    }

    /// SHA-256 of [`Self::canonical`].
    #[must_use]
    pub fn digest(&self) -> [u8; 32] {
        Sha256::digest(self.canonical()).into()
    }
}

/// `value` as compact JSON with object keys sorted, whatever map order
/// `serde_json` was built with.
fn write_sorted(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::from(key.as_str()).to_string());
                out.push(':');
                write_sorted(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_sorted(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

// --- names ------------------------------------------------------------------

/// The catalog name of `command`, e.g. `KILL_RESOURCE`.
#[must_use]
pub(crate) fn command_name(command: &phux_protocol::wire::frame::Command) -> &'static str {
    use phux_protocol::kinds::{self, Carrier};
    // `SESSION_KILL` shares `SESSION_DELETE`'s row, so the row cannot tell them apart.
    if matches!(
        command,
        phux_protocol::wire::frame::Command::SessionKill { .. }
    ) {
        return "SESSION_KILL";
    }
    let rule = kinds::command_rule(command);
    kinds::methods()
        .filter(|method| matches!(method.carrier, Carrier::Command(_)))
        .find(|method| method.rules.iter().any(|row| std::ptr::eq(*row, rule)))
        .map_or("COMMAND", |method| method.name)
}

/// A Terminal in the registry selector grammar: `terminal:3`, or
/// `terminal:<host>/3` on a satellite.
#[must_use]
pub(crate) fn terminal_selector(terminal: &phux_protocol::ids::ResourceId) -> String {
    use phux_protocol::ids::ResourceId;
    match terminal {
        ResourceId::Local { id } => format!("terminal:{id}"),
        ResourceId::Satellite { host, id } => format!("terminal:{}/{id}", host.as_str()),
    }
}

/// The Terminals `command` names, comma-separated, or `None`.
#[must_use]
pub(crate) fn command_target(command: &phux_protocol::wire::frame::Command) -> Option<String> {
    let named: Vec<String> = crate::policy::held_terminals(command)
        .map(terminal_selector)
        .collect();
    (!named.is_empty()).then(|| named.join(","))
}

#[cfg(test)]
mod tests;
