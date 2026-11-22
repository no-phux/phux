//! The messages the `AgentSession` engine's mailboxes carry: appends and
//! their verdicts, bootstrap cuts, and native publication identities.

use bytes::Bytes;
use tokio::sync::oneshot;

use super::record::{RecordError, StreamAsk, StreamEvidence};

/// Publication identity verified by the dedicated native broker.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NativeAppendIdentity {
    /// Host-monotonic publication id.
    pub id: u64,
    /// SHA-256 of the exact normalized update.
    pub fingerprint: String,
}
/// Acceptance stored atomically with the native record append.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NativeAppendAccepted {
    /// Original identity and payload fingerprint.
    pub identity: NativeAppendIdentity,
    /// Original stamps and state/ask evidence.
    pub accepted: AppendAccepted,
}

/// One `APPEND_RESOURCE_OUTPUT` payload handed to the engine.
#[derive(Debug)]
pub struct AppendRequest {
    /// The producer's raw record bytes, already bounded at 64 KiB by the
    /// command decoder.
    pub bytes: Bytes,
    /// Where the stamped header, or the refusal, is returned.
    pub reply: oneshot::Sender<Result<AppendAccepted, AppendRejection>>,
    /// `Some` only after exclusive controller authorization.
    pub publication: Option<NativeAppendIdentity>,
}

/// What the server stamped onto an accepted append.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AppendAccepted {
    /// Sequence of the append's first record; the rest follow contiguously.
    pub first_seq: u64,
    /// Sequence of the append's last record; also the sequence carried by
    /// the live output frame the records shipped in.
    pub last_seq: u64,
    /// The wall-clock milliseconds stamped on every record in this append.
    pub ts_ms: u64,
    /// What the append's records say about the session's state, if
    /// anything. The last state-bearing record in the batch wins.
    pub evidence: Option<StreamEvidence>,
    /// The last question-bearing record's question, for the ask ladder
    /// (ADR-0036).
    pub ask: Option<StreamAsk>,
}

/// Why an append was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendRejection {
    /// A record failed codec validation; nothing was appended.
    Invalid(RecordError),
    /// The engine's mailbox is full or its sequence space is exhausted.
    Overflow(String),
    /// The engine is gone (the session closed under the caller).
    Closed,
}

/// A request for an atomic stream cut, optionally resuming retained records.
#[derive(Debug)]
pub struct BootstrapRequest {
    /// Where the retained records are returned.
    pub reply: oneshot::Sender<AgentSessionBootstrap>,
    /// Last committed consumer record; `None` requests the full retained ring.
    /// Upgrade seal/unseal requests always use `None`.
    pub after_seq: Option<u64>,
    /// Graceful upgrade only (ADR-0032): `Some(true)` seals the stream at
    /// this cut, so no append can be acked and then lost before the re-exec;
    /// `Some(false)` lifts the seal when the upgrade does not happen. `None`
    /// for an ordinary consumer bootstrap.
    pub seal: Option<bool>,
}

/// The replayable state of one agent-session stream at a cut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionBootstrap {
    /// The stream sequence the cut includes; live output resumes at
    /// `base_seq + 1`.
    pub base_seq: u64,
    /// `Some` means `records` is a contiguous suffix after this cursor;
    /// `None` means a full retained replacement, including on eviction.
    pub resume_after_seq: Option<u64>,
    /// Selected records, oldest first, each a complete stamped JSONL line.
    pub records: Vec<Bytes>,
    /// Records retention has evicted; non-zero means the replay starts
    /// mid-session.
    pub dropped: u64,
    /// Whether the stream already accepted `session_end`; a graceful upgrade
    /// carries it so the resumed engine keeps refusing later records.
    pub ended: bool,
    /// Bounded native publication receipts, independent of transcript eviction.
    pub native_publications: Vec<NativeAppendAccepted>,
    /// Old ids below this cursor can never append again.
    pub native_publication_high_water: u64,
}
