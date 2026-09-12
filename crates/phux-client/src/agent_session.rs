//! The `AgentSession` resource from the client side: open, close, emit, log
//! (ADR-0103, `docs/consumers/agents.md` §2 and §4.19).
//!
//! An agent session is the second resource kind the server serves: a
//! producer-fed, ordered stream of `AgentEventsJsonlV1` records bound to the
//! Terminal the agent runs in. This module is the library half of the four
//! `phux agent` verbs the CLI and MCP adapter expose over it, and it speaks
//! only wire verbs the server already has:
//!
//! - [`open`] is `SPAWN_RESOURCE` with the additive kind/parent/provider/
//!   native-id fields;
//! - [`close`] is `KILL_RESOURCE` on the session resource (closing a child
//!   never touches the parent);
//! - [`emit`] is `APPEND_RESOURCE_OUTPUT`, one or more complete JSONL records
//!   per call under the per-record and per-call byte ceilings;
//! - [`log`] attaches to the session resource with `ATTACH_RESOURCE` the way
//!   `phux rec` attaches to a Terminal: the retained records arrive as the
//!   bootstrap transcript and, under `--follow`, live records as
//!   `RESOURCE_OUTPUT` frames until the session closes.
//!
//! Every entry point is gated on [`ServerFeature::ResourceKinds`]: a server
//! that does not advertise the bit has no agent sessions, and the refusal
//! ([`AgentSessionError::Unsupported`]) happens before any frame that server
//! would silently drop.

use serde::{Deserialize, Serialize};

use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::{GroupId, ResourceId};
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, ErrorCode, FrameKind, SpawnError,
    SpawnResource, SpawnResult,
};

use crate::attach::AttachError;
use crate::attach::connection::{Answer, Connection};
use crate::resource;

/// The closed `AgentEventsJsonlV1` record `type` vocabulary (v1).
///
/// The server refuses any other word with `RECORD_INVALID`; the client checks
/// first so a typo costs no round trip and writes nothing.
pub const EVENT_TYPES: &[&str] = &[
    "session_start",
    "prompt",
    "tool_start",
    "tool_end",
    "notification",
    "ask",
    "stop",
    "session_end",
    "state",
    "provider_raw",
];

/// Largest encoded record (one JSON object plus its newline) the server
/// accepts.
pub const MAX_RECORD_BYTES: usize = 16 * 1024;

/// Largest payload one `APPEND_RESOURCE_OUTPUT` carries.
pub const MAX_APPEND_BYTES: usize = 64 * 1024;

/// Correlation ids for the request/response frames this module sends. One
/// connection per verb, so the values only have to be distinct from each
/// other.
const REQUEST_SPAWN: u32 = 1;
const REQUEST_KILL: u32 = 2;
const REQUEST_APPEND: u32 = 3;
const REQUEST_ATTACH: u32 = 4;
const REQUEST_DETACH: u32 = 5;

/// One retained or live record of an agent session's stream, exactly as the
/// wire carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEventRecord {
    /// Dense, server-assigned per-session counter.
    pub seq: u64,
    /// The server's clock at append, milliseconds since the Unix epoch.
    pub ts_ms: u64,
    /// One of [`EVENT_TYPES`] (an open string here so a newer server's word
    /// still decodes).
    #[serde(rename = "type")]
    pub kind: String,
    /// The producer's payload, a JSON object.
    #[serde(default)]
    pub data: serde_json::Value,
}

/// One record to append: the caller's half of an [`AgentEventRecord`]. The
/// server stamps `seq` and `ts_ms`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitRecord {
    kind: String,
    data: serde_json::Map<String, serde_json::Value>,
}

impl EmitRecord {
    /// Build a record, refusing a `type` outside [`EVENT_TYPES`] or a `data`
    /// that is not a JSON object, and a record over [`MAX_RECORD_BYTES`].
    ///
    /// # Errors
    ///
    /// [`AgentSessionError::RecordInvalid`] naming what is wrong.
    pub fn new(kind: &str, data: serde_json::Value) -> Result<Self, AgentSessionError> {
        if !EVENT_TYPES.contains(&kind) {
            return Err(AgentSessionError::RecordInvalid(format!(
                "'{kind}' is not an AgentEventsJsonlV1 record type; use one of {}",
                EVENT_TYPES.join(", ")
            )));
        }
        let serde_json::Value::Object(data) = data else {
            return Err(AgentSessionError::RecordInvalid(
                "record data must be a JSON object".to_owned(),
            ));
        };
        let record = Self {
            kind: kind.to_owned(),
            data,
        };
        let encoded = record.encode_line();
        if encoded.len() > MAX_RECORD_BYTES {
            return Err(AgentSessionError::RecordInvalid(format!(
                "record is {} bytes; the ceiling is {MAX_RECORD_BYTES}",
                encoded.len()
            )));
        }
        Ok(record)
    }

    /// The record `type`.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// One JSONL line: the object plus its newline. `seq` and `ts_ms` are
    /// deliberately absent — the server assigns them and ignores a
    /// producer-supplied value.
    #[must_use]
    pub fn encode_line(&self) -> Vec<u8> {
        let object = serde_json::json!({ "type": self.kind, "data": self.data });
        let mut line = serde_json::to_vec(&object).unwrap_or_default();
        line.push(b'\n');
        line
    }
}

/// What [`open`] returns: the new session and the Terminal it is bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    /// The new `AgentSession` resource.
    pub resource: ResourceId,
    /// Its parent Terminal, echoed from the request.
    pub parent: ResourceId,
}

/// What [`emit`] returns: the header the server stamped on the last record
/// appended, when the reply carried one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Emitted {
    /// `seq` of the last appended record.
    pub seq: Option<u64>,
    /// `ts_ms` of the last appended record.
    pub ts_ms: Option<u64>,
}

/// How much of the stream [`log`] should read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LogOptions {
    /// Keep reading live records after the retained ones until the session
    /// closes, the server goes away, or the sink stops.
    pub follow: bool,
    /// Deliver only the last `n` retained records. `None` delivers every
    /// retained record.
    pub tail: Option<usize>,
}

/// How a [`log`] read ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogEnd {
    /// The retained records were delivered and `follow` was off.
    Retained,
    /// The session closed while following.
    SessionClosed,
    /// The server closed the connection while following.
    Disconnected,
    /// The sink returned `false`.
    Stopped,
}

/// What [`log`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogOutcome {
    /// Retained records the bootstrap carried (before `tail` trimming).
    pub retained: usize,
    /// Records handed to the sink, retained and live.
    pub delivered: usize,
    /// Why the read ended.
    pub end: LogEnd,
}

/// Why an agent session verb did not succeed.
///
/// Every server-side refusal has its own variant so the CLI can map it onto
/// the closed error-code vocabulary (`docs/consumers/agents.md` §5.3) without
/// parsing messages.
#[derive(Debug, thiserror::Error)]
pub enum AgentSessionError {
    /// The server did not advertise `RESOURCE_KINDS`, so agent sessions do
    /// not exist there. Refused before any resource is touched.
    #[error(
        "server does not advertise RESOURCE_KINDS, so agent sessions do not exist there; \
         upgrade the server"
    )]
    Unsupported,
    /// The Terminal has no live `AgentSession` child.
    #[error("{terminal} has no live agent session")]
    NoSession {
        /// The Terminal that was addressed.
        terminal: ResourceId,
    },
    /// The Terminal has more than one live `AgentSession` child; address one
    /// directly.
    #[error("{terminal} has {} live agent sessions: {}", candidates.len(), render_ids(candidates))]
    AmbiguousSession {
        /// The Terminal that was addressed.
        terminal: ResourceId,
        /// Every live child, in snapshot order.
        candidates: Vec<ResourceId>,
    },
    /// The resource exists but is not the kind the operation needs.
    #[error("{resource} is a {actual} resource, not {expected}")]
    WrongKind {
        /// The resource that was addressed.
        resource: ResourceId,
        /// The kind the operation needed (`terminal` / `agent_session`).
        expected: &'static str,
        /// The kind it is.
        actual: String,
    },
    /// `open` named a parent the server does not hold.
    #[error("parent {parent} not found on the server")]
    ParentNotFound {
        /// The parent that was named.
        parent: ResourceId,
    },
    /// `open` named a parent that is not a Terminal-kind resource.
    #[error("parent {parent} is not a Terminal, so it cannot host an agent session")]
    ParentKindMismatch {
        /// The parent that was named.
        parent: ResourceId,
    },
    /// A record is not a valid `AgentEventsJsonlV1` record. Nothing written.
    #[error("record invalid: {0}")]
    RecordInvalid(String),
    /// This client did not open the session, so it may not append.
    #[error(
        "this client is not the producer of {resource}; only the client that opened a session may append to it"
    )]
    NotProducer {
        /// The session that was addressed.
        resource: ResourceId,
    },
    /// The append exceeded a ceiling: the per-call bound, or the session's
    /// retained ring. Nothing written.
    #[error("overflow: {0}")]
    Overflow(String),
    /// The server refused for another reason; the message is its diagnostic.
    #[error("refused: {0}")]
    Refused(String),
    /// Transport or protocol failure.
    #[error(transparent)]
    Transport(#[from] AttachError),
}

fn render_ids(ids: &[ResourceId]) -> String {
    ids.iter()
        .map(crate::selector::format_terminal_id)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether `conn`'s server advertised agent sessions in `HELLO_OK`.
#[must_use]
pub fn supports(conn: &Connection) -> bool {
    conn.negotiated_bootstrap().is_some_and(|negotiated| {
        negotiated
            .server_features
            .contains(ServerFeature::ResourceKinds)
    })
}

/// [`supports`] as a gate: `Ok(())` or [`AgentSessionError::Unsupported`].
///
/// # Errors
///
/// [`AgentSessionError::Unsupported`] when the bit is absent.
pub fn require_support(conn: &Connection) -> Result<(), AgentSessionError> {
    if supports(conn) {
        Ok(())
    } else {
        Err(AgentSessionError::Unsupported)
    }
}

/// Open an `AgentSession` bound to `parent`.
///
/// The caller becomes the session's producer. The server does not
/// deduplicate: a Terminal that already has a live session gets a second
/// one.
///
/// # Errors
///
/// [`AgentSessionError::Unsupported`], the structured spawn refusals
/// ([`AgentSessionError::ParentNotFound`], [`AgentSessionError::ParentKindMismatch`],
/// [`AgentSessionError::Refused`]), or transport failure.
pub async fn open(
    conn: &mut Connection,
    parent: &ResourceId,
    provider: &str,
    native_id: Option<&str>,
) -> Result<Opened, AgentSessionError> {
    require_support(conn)?;
    let frame = FrameKind::SpawnResource {
        request_id: REQUEST_SPAWN,
        group: GroupId::new(1),
        command: None,
        cwd: None,
        env: None,
        term: None,
        satellite: None,
        owner_terminal: None,
        agent_session: None,
        initial_size: None,
        resource: Some(Box::new(
            SpawnResource::agent_session(parent.clone(), provider)
                .with_native_id(native_id.map(str::to_owned)),
        )),
    };
    let (answer, interleaved) = conn.request_spawn(&frame).await?.into_parts();
    crate::state::report_degradation(&interleaved);
    let result = match answer {
        Answer::Ok(result) => result,
        Answer::Err(refusal) => {
            return Err(AgentSessionError::Refused(refusal.to_string()));
        }
    };
    match result {
        SpawnResult::Ok(resource) => Ok(Opened {
            resource,
            parent: parent.clone(),
        }),
        SpawnResult::Err(SpawnError::ParentNotFound) => Err(AgentSessionError::ParentNotFound {
            parent: parent.clone(),
        }),
        SpawnResult::Err(SpawnError::ParentKindMismatch) => {
            Err(AgentSessionError::ParentKindMismatch {
                parent: parent.clone(),
            })
        }
        SpawnResult::Err(SpawnError::UnsupportedKind) => Err(AgentSessionError::Unsupported),
        SpawnResult::Err(other) => Err(AgentSessionError::Refused(format!("{other:?}"))),
        other => Err(AttachError::Protocol(format!(
            "SPAWN_RESOURCE answered with an unrecognised spawn result: {other:?}"
        ))
        .into()),
    }
}

/// Close the `AgentSession` `resource`. The parent Terminal is untouched.
///
/// # Errors
///
/// [`AgentSessionError::Unsupported`], a server refusal, or transport
/// failure.
pub async fn close(conn: &mut Connection, resource: &ResourceId) -> Result<(), AgentSessionError> {
    require_support(conn)?;
    let (result, interleaved) = conn
        .request(
            REQUEST_KILL,
            Command::KillResource {
                terminal_id: resource.clone(),
            },
        )
        .await?
        .into_parts();
    crate::state::report_degradation(&interleaved);
    match result {
        CommandResult::Ok | CommandResult::OkWith(_) => Ok(()),
        CommandResult::Error { code, message } => Err(map_refusal(resource, code, message)),
        other => Err(AttachError::Protocol(crate::explain::explain_unexpected(
            "KILL_RESOURCE",
            &other,
        ))
        .into()),
    }
}

/// Append `records` to the `AgentSession` `resource` in one call.
///
/// Every record is encoded up front and the whole batch is refused before
/// any byte is sent when it exceeds [`MAX_APPEND_BYTES`]; the server's own
/// answer maps onto the typed refusals.
///
/// # Errors
///
/// [`AgentSessionError::RecordInvalid`] / [`AgentSessionError::Overflow`]
/// client-side, the server's `WRONG_RESOURCE_KIND` / `NOT_PRODUCER` /
/// `RECORD_INVALID` / `OVERFLOW` refusals, or transport failure.
pub async fn emit(
    conn: &mut Connection,
    resource: &ResourceId,
    records: &[EmitRecord],
) -> Result<Emitted, AgentSessionError> {
    require_support(conn)?;
    if records.is_empty() {
        return Err(AgentSessionError::RecordInvalid(
            "nothing to append".to_owned(),
        ));
    }
    let mut bytes = Vec::new();
    for record in records {
        bytes.extend(record.encode_line());
    }
    if bytes.len() > MAX_APPEND_BYTES {
        return Err(AgentSessionError::Overflow(format!(
            "{} bytes in one append; the ceiling is {MAX_APPEND_BYTES}",
            bytes.len()
        )));
    }
    let (result, interleaved) = conn
        .request(
            REQUEST_APPEND,
            Command::AppendResourceOutput {
                terminal_id: resource.clone(),
                bytes,
            },
        )
        .await?
        .into_parts();
    crate::state::report_degradation(&interleaved);
    match result {
        CommandResult::OkWith(CommandValue::Json(json)) => Ok(parse_emitted(&json)),
        CommandResult::Ok | CommandResult::OkWith(_) => Ok(Emitted::default()),
        CommandResult::Error { code, message } => Err(map_refusal(resource, code, message)),
        other => Err(AttachError::Protocol(crate::explain::explain_unexpected(
            "APPEND_RESOURCE_OUTPUT",
            &other,
        ))
        .into()),
    }
}

/// The stamped header out of an `OK_WITH(JSON)` append reply, when present.
fn parse_emitted(json: &str) -> Emitted {
    let value: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    Emitted {
        seq: value.get("seq").and_then(serde_json::Value::as_u64),
        ts_ms: value.get("ts_ms").and_then(serde_json::Value::as_u64),
    }
}

/// Map a correlated server refusal onto the typed error.
fn map_refusal(resource: &ResourceId, code: ErrorCode, message: String) -> AgentSessionError {
    match code {
        ErrorCode::WrongResourceKind => AgentSessionError::WrongKind {
            resource: resource.clone(),
            expected: resource::AGENT_SESSION,
            actual: resource::TERMINAL.to_owned(),
        },
        ErrorCode::NotProducer => AgentSessionError::NotProducer {
            resource: resource.clone(),
        },
        ErrorCode::RecordInvalid => AgentSessionError::RecordInvalid(message),
        ErrorCode::Overflow => AgentSessionError::Overflow(message),
        ErrorCode::TerminalNotFound => AgentSessionError::Refused(format!(
            "{} not found: {message}",
            crate::selector::format_terminal_id(resource)
        )),
        _ => AgentSessionError::Refused(format!("{code:?}: {message}")),
    }
}

/// Read the `AgentSession` `resource`'s stream.
///
/// Attaches to the resource as an observer: the retained records arrive as
/// the bootstrap transcript and are handed to `sink` after `READY` (trimmed
/// to `options.tail`); with `options.follow`, live records follow until the
/// session closes, the server goes away, or `sink` returns `false`. Without
/// `follow` the read detaches after the retained records.
///
/// # Errors
///
/// [`AgentSessionError::Unsupported`], the server's refusal of the attach
/// (`WRONG_RESOURCE_KIND` for a Terminal target), a malformed record on the
/// stream, or transport failure.
pub async fn log(
    conn: &mut Connection,
    resource: &ResourceId,
    options: LogOptions,
    mut sink: impl FnMut(AgentEventRecord) -> bool,
) -> Result<LogOutcome, AgentSessionError> {
    require_support(conn)?;
    let (result, primed) = conn
        .request(
            REQUEST_ATTACH,
            Command::AttachResource {
                terminal_id: resource.clone(),
            },
        )
        .await?
        .into_parts();
    if let CommandResult::Error { code, message } = result {
        return Err(map_refusal(resource, code, message));
    }
    conn.bind_terminal(resource).await?;
    if options.follow {
        conn.send(&FrameKind::SubscribeEvents {
            terminal: Some(resource.clone()),
        })
        .await?;
    }

    // Retained records: everything up to BOOTSTRAP_READY. The reference
    // server pushes the whole transcript ahead of the attach ack, so it is
    // normally all in `primed`; a server that acks first is read to READY
    // here the same way.
    let mut reader = Reader::new(resource.clone());
    let mut retained: Vec<AgentEventRecord> = Vec::new();
    let mut ready = false;
    let mut pending = primed.into_iter();
    while !ready {
        let frame = match pending.next() {
            Some(frame) => frame,
            None => conn.recv().await?,
        };
        match reader.absorb(frame)? {
            Absorbed::Records(records) => retained.extend(records),
            Absorbed::Ready => ready = true,
            Absorbed::Closed => {
                // The session ended before its bootstrap completed: whatever
                // was retained is still the honest answer.
                ready = true;
            }
            Absorbed::Nothing => {}
        }
    }

    let retained_count = retained.len();
    let skip = options
        .tail
        .map_or(0, |tail| retained_count.saturating_sub(tail));
    let mut delivered = 0;
    for record in retained.into_iter().skip(skip) {
        delivered += 1;
        if !sink(record) {
            detach(conn, resource).await;
            return Ok(LogOutcome {
                retained: retained_count,
                delivered,
                end: LogEnd::Stopped,
            });
        }
    }
    if !options.follow {
        detach(conn, resource).await;
        return Ok(LogOutcome {
            retained: retained_count,
            delivered,
            end: LogEnd::Retained,
        });
    }

    // Live records, until the session closes or the stream ends. Frames the
    // attach interleaved after READY (a fast producer) are drained first.
    let end = loop {
        let frame = match pending.next() {
            Some(frame) => Ok(frame),
            None => conn.recv().await,
        };
        let frame = match frame {
            Ok(frame) => frame,
            Err(AttachError::Disconnected) => break LogEnd::Disconnected,
            Err(err) => return Err(err.into()),
        };
        match reader.absorb(frame)? {
            Absorbed::Records(records) => {
                let mut stopped = false;
                for record in records {
                    delivered += 1;
                    if !sink(record) {
                        stopped = true;
                        break;
                    }
                }
                if stopped {
                    break LogEnd::Stopped;
                }
            }
            Absorbed::Closed => break LogEnd::SessionClosed,
            Absorbed::Ready | Absorbed::Nothing => {}
        }
    };
    detach(conn, resource).await;
    Ok(LogOutcome {
        retained: retained_count,
        delivered,
        end,
    })
}

/// Best-effort `DETACH_RESOURCE`: idempotent and a no-op on a resource that
/// is already gone (L1 §5.1), so it can never turn a completed read into a
/// failure.
async fn detach(conn: &mut Connection, resource: &ResourceId) {
    let _ = conn
        .send(&FrameKind::Command {
            request_id: REQUEST_DETACH,
            command: Command::DetachResource {
                terminal_id: resource.clone(),
            },
        })
        .await;
    conn.unbind_terminal(resource);
}

/// What one absorbed frame contributed.
enum Absorbed {
    /// Complete records decoded from a bootstrap chunk or a live frame.
    Records(Vec<AgentEventRecord>),
    /// `BOOTSTRAP_READY`: the retained records are complete.
    Ready,
    /// The session closed.
    Closed,
    /// A frame that carries nothing for this read.
    Nothing,
}

/// The frame-to-record projection, with the line buffer that lets a record
/// straddle two bootstrap chunks.
struct Reader {
    resource: ResourceId,
    buffer: Vec<u8>,
}

impl Reader {
    const fn new(resource: ResourceId) -> Self {
        Self {
            resource,
            buffer: Vec::new(),
        }
    }

    fn absorb(&mut self, frame: FrameKind) -> Result<Absorbed, AgentSessionError> {
        match frame {
            FrameKind::BootstrapBegin { terminal_id, .. } if terminal_id == self.resource => {
                // A replacement generation: whatever half-line the previous
                // one left behind is not part of this transcript.
                self.buffer.clear();
                Ok(Absorbed::Nothing)
            }
            FrameKind::BootstrapChunk {
                terminal_id,
                payload,
                ..
            } if terminal_id == self.resource => self.decode(&payload).map(Absorbed::Records),
            FrameKind::BootstrapReady { terminal_id, .. } if terminal_id == self.resource => {
                Ok(Absorbed::Ready)
            }
            FrameKind::ResourceOutput {
                terminal_id, bytes, ..
            } if terminal_id == self.resource => self.decode(&bytes).map(Absorbed::Records),
            FrameKind::Event {
                terminal: Some(terminal),
                event: AgentEvent::ResourceClosed { .. },
            } if terminal == self.resource => Ok(Absorbed::Closed),
            FrameKind::Error {
                request_id: None,
                message,
                ..
            } => {
                crate::state::report_degradation(&[FrameKind::Error {
                    request_id: None,
                    code: ErrorCode::UnsupportedSatelliteRoute,
                    message,
                }]);
                Ok(Absorbed::Nothing)
            }
            FrameKind::Error { message, .. } => Err(AgentSessionError::Refused(message)),
            _ => Ok(Absorbed::Nothing),
        }
    }

    /// Append `bytes` to the line buffer and decode every complete line.
    fn decode(&mut self, bytes: &[u8]) -> Result<Vec<AgentEventRecord>, AgentSessionError> {
        self.buffer.extend_from_slice(bytes);
        let mut records = Vec::new();
        while let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=end).collect();
            let line = &line[..line.len() - 1];
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let record: AgentEventRecord = serde_json::from_slice(line).map_err(|err| {
                AttachError::Protocol(format!(
                    "malformed AgentEventsJsonlV1 record on {}: {err}",
                    crate::selector::format_terminal_id(&self.resource)
                ))
            })?;
            records.push(record);
        }
        Ok(records)
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::*;
    use crate::testkit::{ScriptSpec, ScriptedServer};
    use phux_protocol::caps::ServerFeatureSet;
    use phux_protocol::ids::ResourceKind;
    use tokio::net::UnixListener;

    fn session() -> ResourceId {
        ResourceId::local(9)
    }

    fn parent() -> ResourceId {
        ResourceId::local(7)
    }

    fn kinds() -> ServerFeatureSet {
        ServerFeatureSet::with(&[ServerFeature::ResourceKinds])
    }

    fn serve(
        spec: ScriptSpec,
    ) -> (
        tempfile::TempDir,
        std::path::PathBuf,
        tokio::task::JoinHandle<Vec<FrameKind>>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("agent.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });
        (dir, socket, server)
    }

    #[test]
    fn emit_records_are_validated_before_any_byte_is_sent() {
        assert!(EmitRecord::new("prompt", serde_json::json!({ "chars": 4 })).is_ok());
        assert!(matches!(
            EmitRecord::new("frobnicate", serde_json::json!({})),
            Err(AgentSessionError::RecordInvalid(_))
        ));
        assert!(matches!(
            EmitRecord::new("stop", serde_json::json!("text")),
            Err(AgentSessionError::RecordInvalid(_))
        ));
        let huge = serde_json::json!({ "blob": "x".repeat(MAX_RECORD_BYTES) });
        assert!(matches!(
            EmitRecord::new("provider_raw", huge),
            Err(AgentSessionError::RecordInvalid(_))
        ));
        // `seq` / `ts_ms` are the server's: the encoded line never carries
        // them, whatever the caller put in `data`.
        let line = EmitRecord::new("stop", serde_json::json!({}))
            .unwrap()
            .encode_line();
        let object: serde_json::Value = serde_json::from_slice(&line[..line.len() - 1]).unwrap();
        assert_eq!(object, serde_json::json!({ "type": "stop", "data": {} }));
        assert_eq!(line.last(), Some(&b'\n'));
    }

    #[tokio::test]
    async fn every_verb_is_gated_on_the_resource_kinds_bit() {
        // The default scripted server advertises no features, so each verb
        // must refuse before sending anything the server would drop.
        let (_dir, socket, server) = serve(ScriptSpec::new());
        let mut conn = Connection::connect(&socket).await.unwrap();
        assert!(!supports(&conn));
        assert!(matches!(
            open(&mut conn, &parent(), "claude", None).await,
            Err(AgentSessionError::Unsupported)
        ));
        assert!(matches!(
            close(&mut conn, &session()).await,
            Err(AgentSessionError::Unsupported)
        ));
        let record = EmitRecord::new("stop", serde_json::json!({})).unwrap();
        assert!(matches!(
            emit(&mut conn, &session(), &[record]).await,
            Err(AgentSessionError::Unsupported)
        ));
        assert!(matches!(
            log(&mut conn, &session(), LogOptions::default(), |_| true).await,
            Err(AgentSessionError::Unsupported)
        ));
        drop(conn);
        let seen = server.await.unwrap();
        assert_eq!(
            seen.len(),
            1,
            "only HELLO may reach an unsupporting server: {seen:?}"
        );
    }

    #[tokio::test]
    async fn open_sends_the_kinded_spawn_and_returns_the_new_resource() {
        let spec = ScriptSpec::new()
            .server_features(kinds())
            .spawn_result(SpawnResult::Ok(session()));
        let (_dir, socket, server) = serve(spec);
        let mut conn = Connection::connect(&socket).await.unwrap();
        let opened = open(&mut conn, &parent(), "claude", Some("sess-1"))
            .await
            .unwrap();
        assert_eq!(opened.resource, session());
        assert_eq!(opened.parent, parent());
        drop(conn);
        let seen = server.await.unwrap();
        let Some(FrameKind::SpawnResource {
            resource: Some(resource),
            command,
            ..
        }) = seen.get(1)
        else {
            panic!("SPAWN_RESOURCE with a resource body must follow HELLO, got {seen:?}");
        };
        assert_eq!(resource.kind, ResourceKind::AgentSession);
        assert_eq!(resource.parent, Some(parent()));
        assert_eq!(resource.provider.as_deref(), Some("claude"));
        assert_eq!(resource.native_id.as_deref(), Some("sess-1"));
        assert!(command.is_none(), "an agent session spawns no process");
    }

    #[tokio::test]
    async fn open_maps_the_structured_spawn_refusals() {
        for (error, expect) in [
            (SpawnError::ParentNotFound, "parent_not_found"),
            (SpawnError::ParentKindMismatch, "parent_kind_mismatch"),
        ] {
            let spec = ScriptSpec::new()
                .server_features(kinds())
                .spawn_result(SpawnResult::Err(error));
            let (_dir, socket, server) = serve(spec);
            let mut conn = Connection::connect(&socket).await.unwrap();
            let err = open(&mut conn, &parent(), "claude", None)
                .await
                .unwrap_err();
            match expect {
                "parent_not_found" => {
                    assert!(
                        matches!(err, AgentSessionError::ParentNotFound { .. }),
                        "{err}"
                    );
                }
                _ => assert!(
                    matches!(err, AgentSessionError::ParentKindMismatch { .. }),
                    "{err}"
                ),
            }
            drop(conn);
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn emit_appends_one_jsonl_line_per_record_and_reads_the_stamped_header() {
        let spec = ScriptSpec::new()
            .server_features(kinds())
            .append_result(CommandResult::OkWith(CommandValue::Json(
                r#"{"seq":42,"ts_ms":1757404800123}"#.to_owned(),
            )));
        let (_dir, socket, server) = serve(spec);
        let mut conn = Connection::connect(&socket).await.unwrap();
        let records = [
            EmitRecord::new("prompt", serde_json::json!({ "chars": 4 })).unwrap(),
            EmitRecord::new("stop", serde_json::json!({})).unwrap(),
        ];
        let emitted = emit(&mut conn, &session(), &records).await.unwrap();
        assert_eq!(emitted.seq, Some(42));
        assert_eq!(emitted.ts_ms, Some(1_757_404_800_123));
        drop(conn);
        let seen = server.await.unwrap();
        let Some(FrameKind::Command {
            command: Command::AppendResourceOutput { terminal_id, bytes },
            ..
        }) = seen.get(1)
        else {
            panic!("APPEND_RESOURCE_OUTPUT must follow HELLO, got {seen:?}");
        };
        assert_eq!(*terminal_id, session());
        let text = std::str::from_utf8(bytes).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.ends_with('\n'));
    }

    #[tokio::test]
    async fn emit_maps_every_server_refusal_onto_its_own_variant() {
        for (code, check) in [
            (ErrorCode::WrongResourceKind, "wrong_kind"),
            (ErrorCode::NotProducer, "not_producer"),
            (ErrorCode::RecordInvalid, "record_invalid"),
            (ErrorCode::Overflow, "overflow"),
        ] {
            let spec =
                ScriptSpec::new()
                    .server_features(kinds())
                    .append_result(CommandResult::Error {
                        code,
                        message: "nope".to_owned(),
                    });
            let (_dir, socket, server) = serve(spec);
            let mut conn = Connection::connect(&socket).await.unwrap();
            let record = EmitRecord::new("stop", serde_json::json!({})).unwrap();
            let err = emit(&mut conn, &session(), &[record]).await.unwrap_err();
            let matched = match check {
                "wrong_kind" => matches!(err, AgentSessionError::WrongKind { .. }),
                "not_producer" => matches!(err, AgentSessionError::NotProducer { .. }),
                "record_invalid" => matches!(err, AgentSessionError::RecordInvalid(_)),
                _ => matches!(err, AgentSessionError::Overflow(_)),
            };
            assert!(matched, "{code:?} mapped to {err:?}");
            drop(conn);
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn log_delivers_the_retained_records_then_detaches() {
        let lines = [
            r#"{"seq":1,"ts_ms":10,"type":"session_start","data":{}}"#,
            r#"{"seq":2,"ts_ms":20,"type":"prompt","data":{"chars":4}}"#,
            r#"{"seq":3,"ts_ms":30,"type":"stop","data":{}}"#,
        ];
        let spec = ScriptSpec::new()
            .server_features(kinds())
            .agent_log_bootstrap(&session(), &lines);
        let (_dir, socket, server) = serve(spec);
        let mut conn = Connection::connect(&socket).await.unwrap();
        let mut got = Vec::new();
        let outcome = log(
            &mut conn,
            &session(),
            LogOptions {
                follow: false,
                tail: Some(2),
            },
            |record| {
                got.push(record);
                true
            },
        )
        .await
        .unwrap();
        assert_eq!(outcome.retained, 3);
        assert_eq!(outcome.delivered, 2, "--tail 2 keeps the last two");
        assert_eq!(outcome.end, LogEnd::Retained);
        assert_eq!(got[0].seq, 2);
        assert_eq!(got[0].kind, "prompt");
        assert_eq!(got[0].data["chars"], 4);
        assert_eq!(got[1].seq, 3);
        drop(conn);
        let seen = server.await.unwrap();
        assert!(
            matches!(
                seen.get(1),
                Some(FrameKind::Command {
                    command: Command::AttachResource { terminal_id },
                    ..
                }) if *terminal_id == session()
            ),
            "log attaches the session resource itself: {seen:?}"
        );
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::Command {
                    command: Command::DetachResource { .. },
                    ..
                }
            )),
            "a non-following read detaches: {seen:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|frame| matches!(frame, FrameKind::SubscribeEvents { .. })),
            "a non-following read never subscribes to events"
        );
    }

    #[tokio::test]
    async fn log_follow_streams_live_records_until_the_session_closes() {
        let spec = ScriptSpec::new()
            .server_features(kinds())
            .agent_log_bootstrap(
                &session(),
                &[r#"{"seq":1,"ts_ms":10,"type":"session_start","data":{}}"#],
            )
            .push(FrameKind::ResourceOutput {
                terminal_id: session(),
                stream_id: phux_protocol::ids::StreamId::new(1).expect("stream"),
                bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap"),
                seq: 1,
                bytes: bytes::Bytes::from_static(
                    b"{\"seq\":2,\"ts_ms\":20,\"type\":\"prompt\",\"data\":{}}\n{\"seq\":3,\"ts_ms\":30,\"type\":\"stop\",\"data\":{}}\n",
                ),
            })
            .push(FrameKind::Event {
                terminal: Some(session()),
                event: AgentEvent::ResourceClosed { exit_status: None },
            });
        let (_dir, socket, server) = serve(spec);
        let mut conn = Connection::connect(&socket).await.unwrap();
        let mut seqs = Vec::new();
        let outcome = log(
            &mut conn,
            &session(),
            LogOptions {
                follow: true,
                tail: None,
            },
            |record| {
                seqs.push(record.seq);
                true
            },
        )
        .await
        .unwrap();
        assert_eq!(seqs, [1, 2, 3]);
        assert_eq!(outcome.end, LogEnd::SessionClosed);
        drop(conn);
        server.await.unwrap();
    }

    #[test]
    fn the_reader_reassembles_a_record_split_across_two_chunks() {
        let mut reader = Reader::new(session());
        let first = reader
            .decode(br#"{"seq":1,"ts_ms":10,"type":"sta"#)
            .unwrap();
        assert!(first.is_empty(), "half a line is not a record");
        let second = reader.decode(b"te\",\"data\":{}}\n").unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].kind, "state");
        assert!(matches!(
            reader.decode(b"not json\n"),
            Err(AgentSessionError::Transport(AttachError::Protocol(_)))
        ));
    }
}
