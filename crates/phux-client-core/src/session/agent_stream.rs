//! Typed view of one `AgentSession` resource's `AgentEventsJsonlV1` stream.
//!
//! An `AgentSession` resource never has a terminal replica. Its bootstrap
//! chunks and live output carry complete JSON-lines records, and the kernel
//! parses every byte here before anything leaves it: frontends receive
//! [`AgentEventRecord`]s and a derived [`AgentSessionState`], never raw bytes.

use std::collections::VecDeque;

use serde_json::Value;

/// Upper bound on one record line, matching the producer-side limit.
pub const MAX_RECORD_BYTES: usize = 16 * 1024;

/// Default number of retained records per session log.
pub const DEFAULT_LOG_CAPACITY: usize = 512;

/// The `type` of one record. Open on decode: a newer producer's type is
/// retained as [`Self::Unknown`] rather than rejected, since the closed set
/// is enforced by the server on append, not by the consumer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEventKind {
    /// The provider session opened; `data` may carry `provider` and `native_id`.
    SessionStart,
    /// A human prompt was submitted (`data.length` only by default).
    Prompt,
    /// A tool call began (`data.tool_name`).
    ToolStart,
    /// A tool call finished.
    ToolEnd,
    /// A provider notification (`data.kind`).
    Notification,
    /// The agent is blocked on a human answer.
    Ask,
    /// The agent finished its turn.
    Stop,
    /// The provider session ended.
    SessionEnd,
    /// An explicit lifecycle state (`data.state`).
    State,
    /// Opt-in raw provider payload.
    ProviderRaw,
    /// A type this build does not know.
    Unknown(String),
}

impl AgentEventKind {
    /// Decode the wire `type` word.
    #[must_use]
    pub fn parse(word: &str) -> Self {
        match word {
            "session_start" => Self::SessionStart,
            "prompt" => Self::Prompt,
            "tool_start" => Self::ToolStart,
            "tool_end" => Self::ToolEnd,
            "notification" => Self::Notification,
            "ask" => Self::Ask,
            "stop" => Self::Stop,
            "session_end" => Self::SessionEnd,
            "state" => Self::State,
            "provider_raw" => Self::ProviderRaw,
            other => Self::Unknown(other.to_owned()),
        }
    }

    /// The wire `type` word.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::SessionStart => "session_start",
            Self::Prompt => "prompt",
            Self::ToolStart => "tool_start",
            Self::ToolEnd => "tool_end",
            Self::Notification => "notification",
            Self::Ask => "ask",
            Self::Stop => "stop",
            Self::SessionEnd => "session_end",
            Self::State => "state",
            Self::ProviderRaw => "provider_raw",
            Self::Unknown(word) => word,
        }
    }
}

/// One decoded `AgentEventsJsonlV1` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEventRecord {
    /// Server-assigned stream sequence.
    pub seq: u64,
    /// Server-assigned append time, milliseconds since the Unix epoch.
    pub ts_ms: u64,
    /// The record type.
    pub kind: AgentEventKind,
    /// The type-specific payload; an empty object when absent.
    pub data: Value,
}

impl AgentEventRecord {
    /// A string field of `data`, if present.
    #[must_use]
    pub fn data_str(&self, field: &str) -> Option<&str> {
        self.data.get(field).and_then(Value::as_str)
    }
}

/// Why a stream payload could not be decoded into records.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentRecordError {
    /// A line exceeded [`MAX_RECORD_BYTES`].
    #[error("agent record of {0} bytes exceeds the {MAX_RECORD_BYTES} byte limit")]
    Oversize(usize),
    /// A line was not valid JSON.
    #[error("agent record is not valid JSON: {0}")]
    Json(String),
    /// A line was JSON but not an object.
    #[error("agent record is not a JSON object")]
    NotAnObject,
    /// A required field was missing or had the wrong type.
    #[error("agent record is missing field `{0}`")]
    MissingField(&'static str),
}

/// Decode every complete line of `bytes`.
///
/// Empty lines (including the trailing newline) are skipped. A payload with
/// no records at all decodes to an empty vector.
///
/// # Errors
/// Any malformed line fails the whole payload; the caller retires the
/// stream generation rather than skipping the line, so a frontend never sees
/// a log with a silent hole in it.
pub fn parse_records(bytes: &[u8]) -> Result<Vec<AgentEventRecord>, AgentRecordError> {
    let mut records = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        let line = trim_ascii(line);
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_RECORD_BYTES {
            return Err(AgentRecordError::Oversize(line.len()));
        }
        records.push(parse_record(line)?);
    }
    Ok(records)
}

fn trim_ascii(line: &[u8]) -> &[u8] {
    let start = line
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(line.len());
    let end = line
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    &line[start..end]
}

fn parse_record(line: &[u8]) -> Result<AgentEventRecord, AgentRecordError> {
    let value: Value =
        serde_json::from_slice(line).map_err(|error| AgentRecordError::Json(error.to_string()))?;
    let Value::Object(mut object) = value else {
        return Err(AgentRecordError::NotAnObject);
    };
    let seq = object
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or(AgentRecordError::MissingField("seq"))?;
    let ts_ms = object
        .get("ts_ms")
        .and_then(Value::as_u64)
        .ok_or(AgentRecordError::MissingField("ts_ms"))?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .map(AgentEventKind::parse)
        .ok_or(AgentRecordError::MissingField("type"))?;
    let data = match object.remove("data") {
        None | Some(Value::Null) => Value::Object(serde_json::Map::new()),
        Some(data @ Value::Object(_)) => data,
        Some(_) => return Err(AgentRecordError::MissingField("data")),
    };
    Ok(AgentEventRecord {
        seq,
        ts_ms,
        kind,
        data,
    })
}

/// Lifecycle status derived from the record stream.
///
/// Idle is not derived here: an agent that stopped emitting records is not
/// thereby idle, and the server-side detector owns that judgement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentSessionStatus {
    /// No record has said anything about state yet.
    #[default]
    Unknown,
    /// The agent declared itself idle through an explicit `state` record.
    Idle,
    /// A prompt or tool call is in flight.
    Working,
    /// The agent is waiting on a human.
    Blocked,
    /// The agent finished its turn.
    Done,
    /// The provider session ended; the state is retracted.
    Ended,
}

impl AgentSessionStatus {
    /// Decode an explicit `state` word; anything outside the vocabulary is
    /// [`Self::Unknown`].
    #[must_use]
    pub fn parse(word: &str) -> Self {
        match word {
            "idle" => Self::Idle,
            "working" => Self::Working,
            "blocked" => Self::Blocked,
            "done" => Self::Done,
            _ => Self::Unknown,
        }
    }

    /// The kebab-case display word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Ended => "ended",
        }
    }
}

/// Identity and lifecycle of one `AgentSession`, folded from its records.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentSessionState {
    /// Provider slug (`claude`, `codex`, ...).
    pub provider: Option<String>,
    /// Opaque provider session id.
    pub native_id: Option<String>,
    /// Derived lifecycle status.
    pub status: AgentSessionStatus,
}

impl AgentSessionState {
    /// Fold one record into the state. Returns `true` when anything changed.
    pub fn fold(&mut self, record: &AgentEventRecord) -> bool {
        let before = self.clone();
        match &record.kind {
            AgentEventKind::SessionStart => {
                if let Some(provider) = record.data_str("provider") {
                    self.provider = Some(provider.to_owned());
                }
                if let Some(native_id) = record.data_str("native_id") {
                    self.native_id = Some(native_id.to_owned());
                }
                if self.status == AgentSessionStatus::Ended {
                    self.status = AgentSessionStatus::Unknown;
                }
            }
            AgentEventKind::Prompt | AgentEventKind::ToolStart => {
                self.status = AgentSessionStatus::Working;
            }
            AgentEventKind::Ask => self.status = AgentSessionStatus::Blocked,
            AgentEventKind::Notification => {
                if matches!(record.data_str("kind"), Some("permission" | "elicitation")) {
                    self.status = AgentSessionStatus::Blocked;
                }
            }
            AgentEventKind::Stop => self.status = AgentSessionStatus::Done,
            AgentEventKind::SessionEnd => self.status = AgentSessionStatus::Ended,
            AgentEventKind::State => {
                if let Some(word) = record.data_str("state") {
                    self.status = AgentSessionStatus::parse(word);
                }
            }
            AgentEventKind::ToolEnd | AgentEventKind::ProviderRaw | AgentEventKind::Unknown(_) => {}
        }
        *self != before
    }
}

/// A bounded, ordered retention of the newest records.
#[derive(Debug, Clone)]
pub struct AgentLog {
    records: VecDeque<AgentEventRecord>,
    capacity: usize,
}

impl Default for AgentLog {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_LOG_CAPACITY)
    }
}

impl AgentLog {
    /// A log retaining at most `capacity` records (at least one).
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            records: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Append one record, evicting the oldest past capacity.
    pub fn push(&mut self, record: AgentEventRecord) {
        if self.records.len() == self.capacity {
            self.records.pop_front();
        }
        self.records.push_back(record);
    }

    /// Drop every retained record.
    pub fn clear(&mut self) {
        self.records.clear();
    }

    /// Retained records, oldest first.
    #[must_use]
    pub fn records(&self) -> impl ExactSizeIterator<Item = &AgentEventRecord> + '_ {
        self.records.iter()
    }

    /// Number of retained records.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether nothing is retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The newest retained record.
    #[must_use]
    pub fn last(&self) -> Option<&AgentEventRecord> {
        self.records.back()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn line(seq: u64, kind: &str, data: &str) -> String {
        format!(
            "{{\"seq\":{seq},\"ts_ms\":{},\"type\":\"{kind}\",\"data\":{data}}}\n",
            seq * 10
        )
    }

    #[test]
    fn parses_multiple_lines_and_skips_blanks() {
        let payload = format!(
            "{}\n{}",
            line(
                1,
                "session_start",
                r#"{"provider":"claude","native_id":"abc"}"#
            ),
            line(2, "prompt", r#"{"length":12}"#)
        );
        let records = parse_records(payload.as_bytes()).expect("parse");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].kind, AgentEventKind::SessionStart);
        assert_eq!(records[0].data_str("provider"), Some("claude"));
        assert_eq!(records[1].seq, 2);
        assert_eq!(records[1].kind, AgentEventKind::Prompt);
    }

    #[test]
    fn missing_data_defaults_to_an_empty_object() {
        let records = parse_records(br#"{"seq":1,"ts_ms":1,"type":"stop"}"#).expect("parse");
        assert_eq!(records[0].data, Value::Object(serde_json::Map::new()));
    }

    #[test]
    fn unknown_type_is_retained_not_rejected() {
        let records = parse_records(line(3, "future_thing", "{}").as_bytes()).expect("parse");
        assert_eq!(
            records[0].kind,
            AgentEventKind::Unknown("future_thing".to_owned())
        );
        assert_eq!(records[0].kind.as_str(), "future_thing");
    }

    #[test]
    fn malformed_lines_fail_the_payload() {
        assert_eq!(
            parse_records(b"[1,2,3]"),
            Err(AgentRecordError::NotAnObject)
        );
        assert_eq!(
            parse_records(br#"{"ts_ms":1,"type":"stop"}"#),
            Err(AgentRecordError::MissingField("seq"))
        );
        assert_eq!(
            parse_records(br#"{"seq":1,"ts_ms":1}"#),
            Err(AgentRecordError::MissingField("type"))
        );
        assert!(matches!(
            parse_records(b"{not json"),
            Err(AgentRecordError::Json(_))
        ));
        let oversize = format!(
            "{{\"seq\":1,\"ts_ms\":1,\"type\":\"stop\",\"data\":{{\"pad\":\"{}\"}}}}",
            "x".repeat(MAX_RECORD_BYTES)
        );
        assert!(matches!(
            parse_records(oversize.as_bytes()),
            Err(AgentRecordError::Oversize(_))
        ));
    }

    #[test]
    #[allow(
        clippy::cognitive_complexity,
        reason = "one linear walk through the v1 derivation table"
    )]
    fn state_derivation_follows_the_v1_rules() {
        let mut state = AgentSessionState::default();
        let fold = |state: &mut AgentSessionState, seq, kind, data| {
            let records = parse_records(line(seq, kind, data).as_bytes()).expect("parse");
            state.fold(&records[0])
        };
        assert!(fold(
            &mut state,
            1,
            "session_start",
            r#"{"provider":"claude","native_id":"s-1"}"#
        ));
        assert_eq!(state.provider.as_deref(), Some("claude"));
        assert_eq!(state.native_id.as_deref(), Some("s-1"));
        assert_eq!(state.status, AgentSessionStatus::Unknown);
        assert!(fold(&mut state, 2, "prompt", "{}"));
        assert_eq!(state.status, AgentSessionStatus::Working);
        assert!(!fold(
            &mut state,
            3,
            "tool_start",
            r#"{"tool_name":"Read"}"#
        ));
        assert!(!fold(&mut state, 4, "tool_end", "{}"));
        assert!(!fold(&mut state, 5, "notification", r#"{"kind":"info"}"#));
        assert!(fold(
            &mut state,
            6,
            "notification",
            r#"{"kind":"permission"}"#
        ));
        assert_eq!(state.status, AgentSessionStatus::Blocked);
        assert!(fold(&mut state, 7, "prompt", "{}"));
        assert!(fold(&mut state, 8, "ask", r#"{"question":"?"}"#));
        assert_eq!(state.status, AgentSessionStatus::Blocked);
        assert!(fold(&mut state, 9, "stop", "{}"));
        assert_eq!(state.status, AgentSessionStatus::Done);
        assert!(fold(&mut state, 10, "state", r#"{"state":"idle"}"#));
        assert_eq!(state.status, AgentSessionStatus::Idle);
        assert!(fold(&mut state, 11, "state", r#"{"state":"weird"}"#));
        assert_eq!(state.status, AgentSessionStatus::Unknown);
        assert!(fold(&mut state, 12, "session_end", "{}"));
        assert_eq!(state.status, AgentSessionStatus::Ended);
        assert!(fold(&mut state, 13, "session_start", "{}"));
        assert_eq!(state.status, AgentSessionStatus::Unknown);
        assert_eq!(state.provider.as_deref(), Some("claude"));
    }

    #[test]
    fn log_is_bounded_and_ordered() {
        let mut log = AgentLog::with_capacity(2);
        for seq in 1..=3 {
            let records = parse_records(line(seq, "stop", "{}").as_bytes()).expect("parse");
            log.push(records.into_iter().next().expect("one"));
        }
        let seqs: Vec<u64> = log.records().map(|record| record.seq).collect();
        assert_eq!(seqs, vec![2, 3]);
        assert_eq!(log.last().map(|record| record.seq), Some(3));
        assert_eq!(log.len(), 2);
        log.clear();
        assert!(log.is_empty());
    }
}
