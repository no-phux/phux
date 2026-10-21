//! The `AgentSession` resource from the client side (ADR-0103).
//!
//! [`open`] is `SPAWN_RESOURCE` with a kinded body, [`close`] is
//! `KILL_RESOURCE`, [`emit`] is `APPEND_RESOURCE_OUTPUT` of JSONL records,
//! and [`log`] is an observer attach whose bootstrap carries the retained
//! records, then live `RESOURCE_OUTPUT` under `follow`.
//!
//! Every verb is gated on [`ServerFeature::ResourceKinds`] before any frame
//! a server without it would silently drop.

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

/// The closed `AgentEventsJsonlV1` record `type` vocabulary (v1), checked
/// client-side so a typo writes nothing.
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

/// Correlation ids; one connection per verb, so they need only be distinct.
const REQUEST_SPAWN: u32 = 1;
const REQUEST_KILL: u32 = 2;
const REQUEST_APPEND: u32 = 3;
const REQUEST_ATTACH: u32 = 4;
const REQUEST_DETACH: u32 = 5;

mod log;
pub use log::log;
#[cfg(test)]
use log::{Absorbed, Reader};

/// One retained or live record of an agent session's stream, exactly as the
/// wire carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEventRecord {
    /// Dense, server-assigned per-session counter.
    pub seq: u64,
    /// The server's clock at append, milliseconds since the Unix epoch.
    pub ts_ms: u64,
    /// One of [`EVENT_TYPES`] (open here, so a newer word still decodes).
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

    /// One JSONL line; `seq` and `ts_ms` are the server's to assign.
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

/// Why an agent session verb did not succeed; one variant per refusal so
/// the CLI maps codes without parsing messages.
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
///
/// # Errors
///
/// [`AgentSessionError::Unsupported`] when the bit is absent.
pub fn require_support(conn: &Connection) -> Result<(), AgentSessionError> {
    let supported = conn.negotiated_bootstrap().is_some_and(|negotiated| {
        negotiated
            .server_features
            .contains(ServerFeature::ResourceKinds)
    });
    if supported {
        Ok(())
    } else {
        Err(AgentSessionError::Unsupported)
    }
}

/// Open an `AgentSession` bound to `parent`; the caller becomes its
/// producer. The server does not deduplicate sessions per Terminal.
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
                operation_id: None,
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

/// Append `records` to `resource` in one call; a batch over
/// [`MAX_APPEND_BYTES`] is refused before any byte is sent.
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

#[cfg(test)]
mod tests;
