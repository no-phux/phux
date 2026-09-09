//! `AgentEventsJsonlV1` record validation, stamping, and state derivation.
//!
//! One UTF-8 JSON object per line, at most [`MAX_RECORD_BYTES`] each,
//! `{"seq":u64,"ts_ms":u64,"type":<str>,"data":{...}}` (ADR-0103 §2). The
//! server parses the codec it serves for exactly two reasons: to refuse a
//! record that would corrupt the stream, and to derive the session's
//! lifecycle state. Neither reads further into `data` than the two keys
//! ADR-0103 §5 names.
//!
//! `seq` and `ts_ms` belong to the server. A producer is a short-lived hook
//! process that can race a sibling, so whatever it supplies for either is
//! discarded and [`ValidRecord::stamp`] writes the server's own values. The
//! stamped line is canonical — `{"seq":…,"ts_ms":…,"type":…,"data":{…}}` in
//! that order, and no other top-level key — so every consumer reads one
//! shape whatever the producer sent.

use std::fmt;

use serde_json::{Map, Value};

/// Largest single record the codec accepts, in bytes (ADR-0103 §2).
pub const MAX_RECORD_BYTES: usize = 16 * 1024;

/// The closed v1 `type` vocabulary (ADR-0103 §2). A record naming anything
/// else is [`RecordError::UnknownType`]; a new type is a codec revision.
pub const RECORD_TYPES: [&str; 10] = [
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

/// What one accepted record says about the session's lifecycle
/// (ADR-0103 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEvidence {
    /// The session is doing work.
    Working,
    /// The session is waiting on a human.
    Blocked,
    /// The session finished its turn.
    Done,
    /// The session ended; the derived state is withdrawn.
    Retract,
}

/// Why a record was refused. Every variant is `RECORD_INVALID` on the wire;
/// the text rides the `COMMAND_RESULT` message so a producer can fix its
/// emitter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordError {
    /// The append carried no record at all.
    Empty,
    /// The append bytes are not UTF-8.
    NotUtf8,
    /// One record exceeds [`MAX_RECORD_BYTES`].
    Oversized {
        /// The record's length in bytes.
        len: usize,
    },
    /// One line is not a JSON object.
    NotAnObject,
    /// A record carries no `type`, or a `type` that is not a string.
    MissingType,
    /// A record's `type` is outside the v1 vocabulary.
    UnknownType {
        /// The type the producer sent, truncated for the reply.
        found: String,
    },
    /// A record's `data` is present but is not an object.
    DataNotObject,
    /// A record arrived after the session's `session_end`.
    AfterSessionEnd,
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("an append must carry at least one record"),
            Self::NotUtf8 => f.write_str("agent-event records must be UTF-8"),
            Self::Oversized { len } => write!(
                f,
                "a record of {len} bytes exceeds the {MAX_RECORD_BYTES}-byte codec limit"
            ),
            Self::NotAnObject => f.write_str("each record line must be a JSON object"),
            Self::MissingType => f.write_str("each record needs a string `type`"),
            Self::UnknownType { found } => write!(
                f,
                "`{found}` is not an AgentEventsJsonlV1 type; the v1 set is {}",
                RECORD_TYPES.join(", ")
            ),
            Self::DataNotObject => f.write_str("a record's `data` must be an object"),
            Self::AfterSessionEnd => {
                f.write_str("the session already ended; no further records are accepted")
            }
        }
    }
}

/// A pending question a blocking record carried, for the ask ladder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamAsk {
    /// Producer-chosen question id; empty when it named none.
    pub id: String,
    /// The question text shown to a human.
    pub question: String,
    /// Answers the producer offered, in its own order.
    pub suggestions: Vec<String>,
}

/// One validated record, ready for the server to stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidRecord {
    /// The record's `type`, already checked against [`RECORD_TYPES`].
    pub record_type: String,
    /// The record's `data` object; an absent `data` normalizes to empty.
    pub data: Map<String, Value>,
}

impl ValidRecord {
    /// What this record says about the session's state, or `None` when it
    /// is not state-bearing (ADR-0103 §5).
    ///
    /// `notification` is evidence only for the two kinds that block on a
    /// human; every other notification is narration. A `state` record is
    /// the `REPORT_AGENT_STATE` fallback's synthesized form, so its
    /// `data.state` word is read directly.
    #[must_use]
    pub fn evidence(&self) -> Option<StreamEvidence> {
        match self.record_type.as_str() {
            "prompt" | "tool_start" => Some(StreamEvidence::Working),
            "ask" => Some(StreamEvidence::Blocked),
            "notification" => match self.data.get("kind").and_then(Value::as_str) {
                Some("permission" | "elicitation") => Some(StreamEvidence::Blocked),
                _ => None,
            },
            "stop" => Some(StreamEvidence::Done),
            "session_end" => Some(StreamEvidence::Retract),
            "state" => match self.data.get("state").and_then(Value::as_str) {
                Some("working") => Some(StreamEvidence::Working),
                Some("blocked") => Some(StreamEvidence::Blocked),
                Some("done") => Some(StreamEvidence::Done),
                _ => None,
            },
            _ => None,
        }
    }

    /// The pending question this record carries, if it carries one.
    ///
    /// The ask ladder (ADR-0036) is a separate ledger from the state one:
    /// "who is asking" and "what state is the pane in" have different
    /// sources and different retraction rules, so a record can feed both,
    /// one, or neither. Only a record that actually names a question does —
    /// an `ask` with no text is a state edge, not a question a human can
    /// answer.
    ///
    /// Shaped as plain strings rather than the ask ledger's own type: the
    /// engine validates a codec, and what the server does with a question is
    /// the runtime's concern.
    #[must_use]
    pub fn ask(&self) -> Option<StreamAsk> {
        if self.evidence() != Some(StreamEvidence::Blocked) {
            return None;
        }
        let question = self.data.get("question").and_then(Value::as_str)?;
        if question.is_empty() {
            return None;
        }
        Some(StreamAsk {
            id: self
                .data
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            question: question.to_owned(),
            suggestions: self
                .data
                .get("suggestions")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    /// `true` iff this record ends the session.
    #[must_use]
    pub fn ends_session(&self) -> bool {
        self.record_type == "session_end"
    }

    /// The canonical stamped line, with the trailing newline that makes the
    /// stream JSONL. `seq` and `ts_ms` are the server's.
    #[must_use]
    pub fn stamp(&self, seq: u64, ts_ms: u64) -> bytes::Bytes {
        let mut line = String::with_capacity(64);
        line.push_str("{\"seq\":");
        line.push_str(&seq.to_string());
        line.push_str(",\"ts_ms\":");
        line.push_str(&ts_ms.to_string());
        line.push_str(",\"type\":");
        line.push_str(&Value::String(self.record_type.clone()).to_string());
        line.push_str(",\"data\":");
        line.push_str(&Value::Object(self.data.clone()).to_string());
        line.push_str("}\n");
        bytes::Bytes::from(line)
    }
}

/// Validate one `APPEND_RESOURCE_OUTPUT` payload into its records.
///
/// Newline-separated, and the trailing newline is optional: a producer
/// piping one record through `phux agent emit` and one shipping a batch
/// both round-trip. A blank segment is skipped rather than refused —
/// concatenating two producers' output must not be an error — but an
/// append that yields no record at all is [`RecordError::Empty`].
///
/// `already_ended` carries whether a `session_end` has already been
/// accepted; the check lives here, not at the call site, so a batch whose
/// second record follows its own `session_end` is refused too.
///
/// # Errors
///
/// The first [`RecordError`] the payload produces. Validation is
/// all-or-nothing by construction: the caller commits nothing until this
/// returns `Ok`.
pub fn validate(bytes: &[u8], already_ended: bool) -> Result<Vec<ValidRecord>, RecordError> {
    let text = std::str::from_utf8(bytes).map_err(|_| RecordError::NotUtf8)?;
    let mut ended = already_ended;
    let mut records = Vec::new();
    for line in text.split('\n') {
        let line = line.trim_matches(|c: char| c == '\r' || c == ' ' || c == '\t');
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_RECORD_BYTES {
            return Err(RecordError::Oversized { len: line.len() });
        }
        if ended {
            return Err(RecordError::AfterSessionEnd);
        }
        let record = parse_record(line)?;
        ended = record.ends_session();
        records.push(record);
    }
    if records.is_empty() {
        return Err(RecordError::Empty);
    }
    Ok(records)
}

/// Parse and check one non-empty line.
fn parse_record(line: &str) -> Result<ValidRecord, RecordError> {
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(line) else {
        return Err(RecordError::NotAnObject);
    };
    let Some(record_type) = object.get("type").and_then(Value::as_str) else {
        return Err(RecordError::MissingType);
    };
    if !RECORD_TYPES.contains(&record_type) {
        return Err(RecordError::UnknownType {
            found: record_type.chars().take(64).collect(),
        });
    }
    let data = match object.get("data") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(data)) => data.clone(),
        Some(_) => return Err(RecordError::DataNotObject),
    };
    Ok(ValidRecord {
        record_type: record_type.to_owned(),
        data,
    })
}

/// Wall-clock milliseconds since the Unix epoch, saturating at `0` on a
/// clock set before it.
#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(line: &str) -> Result<ValidRecord, RecordError> {
        validate(line.as_bytes(), false).map(|mut r| r.remove(0))
    }

    #[test]
    fn a_batch_splits_on_newlines_with_an_optional_trailing_one() {
        let payload = "{\"type\":\"prompt\"}\n{\"type\":\"stop\",\"data\":{}}";
        let records = validate(payload.as_bytes(), false).expect("two records");
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].record_type, "stop");
    }

    #[test]
    fn blank_segments_are_skipped_and_an_empty_append_is_refused() {
        assert_eq!(
            validate(b"\n\n  \n", false),
            Err(RecordError::Empty),
            "whitespace alone carries no record"
        );
        assert_eq!(validate(b"", false), Err(RecordError::Empty));
        let records = validate(b"\n{\"type\":\"stop\"}\n\n", false).expect("one record");
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn the_closed_type_set_is_enforced() {
        assert_eq!(
            one("{\"type\":\"tool_middle\"}"),
            Err(RecordError::UnknownType {
                found: "tool_middle".to_owned()
            })
        );
        for record_type in RECORD_TYPES {
            one(&format!("{{\"type\":\"{record_type}\"}}"))
                .unwrap_or_else(|e| panic!("{record_type} is v1: {e}"));
        }
    }

    #[test]
    fn a_record_needs_a_string_type_an_object_body_and_an_object_data() {
        assert_eq!(one("[1,2]"), Err(RecordError::NotAnObject));
        assert_eq!(one("not json"), Err(RecordError::NotAnObject));
        assert_eq!(one("{\"kind\":\"prompt\"}"), Err(RecordError::MissingType));
        assert_eq!(one("{\"type\":7}"), Err(RecordError::MissingType));
        assert_eq!(
            one("{\"type\":\"prompt\",\"data\":[]}"),
            Err(RecordError::DataNotObject)
        );
        assert_eq!(
            one("{\"type\":\"prompt\",\"data\":null}")
                .expect("null data normalizes")
                .data
                .len(),
            0
        );
    }

    #[test]
    fn oversize_and_post_session_end_records_are_refused() {
        let big = format!(
            "{{\"type\":\"provider_raw\",\"data\":{{\"b\":\"{}\"}}}}",
            "x".repeat(MAX_RECORD_BYTES)
        );
        assert!(matches!(
            validate(big.as_bytes(), false),
            Err(RecordError::Oversized { .. })
        ));
        assert_eq!(
            validate(b"{\"type\":\"prompt\"}", true),
            Err(RecordError::AfterSessionEnd)
        );
        assert_eq!(
            validate(b"{\"type\":\"session_end\"}\n{\"type\":\"prompt\"}", false),
            Err(RecordError::AfterSessionEnd),
            "a batch is checked against its own session_end"
        );
    }

    #[test]
    fn state_derivation_follows_adr_0103() {
        let cases = [
            ("{\"type\":\"prompt\"}", Some(StreamEvidence::Working)),
            ("{\"type\":\"tool_start\"}", Some(StreamEvidence::Working)),
            ("{\"type\":\"tool_end\"}", None),
            ("{\"type\":\"ask\"}", Some(StreamEvidence::Blocked)),
            (
                "{\"type\":\"notification\",\"data\":{\"kind\":\"permission\"}}",
                Some(StreamEvidence::Blocked),
            ),
            (
                "{\"type\":\"notification\",\"data\":{\"kind\":\"elicitation\"}}",
                Some(StreamEvidence::Blocked),
            ),
            (
                "{\"type\":\"notification\",\"data\":{\"kind\":\"progress\"}}",
                None,
            ),
            ("{\"type\":\"stop\"}", Some(StreamEvidence::Done)),
            ("{\"type\":\"session_end\"}", Some(StreamEvidence::Retract)),
            ("{\"type\":\"session_start\"}", None),
            (
                "{\"type\":\"state\",\"data\":{\"state\":\"blocked\"}}",
                Some(StreamEvidence::Blocked),
            ),
            ("{\"type\":\"state\",\"data\":{\"state\":\"idle\"}}", None),
            ("{\"type\":\"provider_raw\"}", None),
        ];
        for (line, expected) in cases {
            assert_eq!(one(line).expect("valid").evidence(), expected, "{line}");
        }
    }

    #[test]
    fn stamping_is_canonical_and_ignores_producer_seq_and_time() {
        let record = one("{\"seq\":99,\"ts_ms\":1,\"type\":\"prompt\",\"data\":{\"len\":4}}")
            .expect("valid");
        assert_eq!(
            record.stamp(7, 1_700_000_000_000),
            bytes::Bytes::from_static(
                b"{\"seq\":7,\"ts_ms\":1700000000000,\"type\":\"prompt\",\"data\":{\"len\":4}}\n"
            )
        );
    }
}
