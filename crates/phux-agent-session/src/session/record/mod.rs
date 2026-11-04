//! `AgentEventsJsonlV1` record validation, stamping, and state derivation.
//!
//! One JSON object per line, at most [`MAX_RECORD_BYTES`]:
//! `{"seq":u64,"ts_ms":u64,"type":<str>,"data":{...}}` (ADR-0103 §2). The
//! server parses only to refuse corrupting records and to derive state
//! (ADR-0103 §5). `seq` and `ts_ms` are the server's: [`ValidRecord::stamp`]
//! discards the producer's and writes one canonical shape.

use std::fmt;

use bytes::{Bytes, BytesMut};
use serde::Serialize;
use serde_json::{Map, Value};

/// Largest single record the codec accepts, including LF (L1 §7.1).
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// Why a record was refused (`RECORD_INVALID` on the wire; the text helps
/// the producer fix its emitter).
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// The session state this record asserts, if any (ADR-0103 §5). Only
    /// the two human-blocking notification kinds count; a `state` record's
    /// `data.state` is read directly.
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

    /// The question this record carries, if any (a separate ledger from
    /// state, ADR-0036). An `ask` without text is a state edge, not a
    /// question.
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
    pub fn stamp(&self, seq: u64, ts_ms: u64) -> Bytes {
        let mut buf = BytesMut::with_capacity(64 + self.record_type.len());
        self.write_stamp(seq, ts_ms, &mut buf);
        buf.freeze()
    }

    /// Stamp a record and enforce the codec limit on the canonical wire
    /// shape, including its terminating newline.
    #[cfg(test)]
    pub(super) fn stamp_bounded(&self, seq: u64, ts_ms: u64) -> Result<Bytes, RecordError> {
        let line = self.stamp(seq, ts_ms);
        let len = line.len();
        if len > MAX_RECORD_BYTES {
            return Err(RecordError::Oversized { len });
        }
        Ok(line)
    }

    /// Append the canonical line to `out`. Returns the number of bytes written.
    pub(super) fn write_stamp(&self, seq: u64, ts_ms: u64, out: &mut BytesMut) -> usize {
        let start = out.len();
        out.extend_from_slice(b"{\"seq\":");
        push_u64(out, seq);
        out.extend_from_slice(b",\"ts_ms\":");
        push_u64(out, ts_ms);
        out.extend_from_slice(b",\"type\":");
        append_json(out, &self.record_type);
        out.extend_from_slice(b",\"data\":");
        append_json(out, &self.data);
        out.extend_from_slice(b"}\n");
        out.len() - start
    }
}

/// Stamp every record into one buffer. The caller slices each line out of
/// the returned [`Bytes`]. Nothing is retained when any line exceeds
/// [`MAX_RECORD_BYTES`].
pub(super) fn stamp_batch(
    records: &[ValidRecord],
    first_seq: u64,
    ts_ms: u64,
    capacity: usize,
) -> Result<(Bytes, Vec<std::ops::Range<usize>>), RecordError> {
    let mut payload = BytesMut::with_capacity(capacity);
    let mut ranges = Vec::with_capacity(records.len());
    for (index, entry) in records.iter().enumerate() {
        let start = payload.len();
        let len = entry.write_stamp(first_seq + index as u64, ts_ms, &mut payload);
        if len > MAX_RECORD_BYTES {
            return Err(RecordError::Oversized { len });
        }
        ranges.push(start..start + len);
    }
    Ok((payload.freeze(), ranges))
}

/// Decimal `value` with no leading zeros, written straight into `out`.
fn push_u64(out: &mut BytesMut, mut value: u64) {
    let mut buf = [0_u8; 20];
    let mut i = buf.len();
    loop {
        i -= 1;
        let digit = u8::try_from(value % 10).unwrap_or(0);
        buf[i] = b'0' + digit;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    out.extend_from_slice(&buf[i..]);
}

/// `BytesMut` implements [`std::fmt::Write`], not [`std::io::Write`].
struct BufWriter<'a>(&'a mut BytesMut);

impl std::io::Write for BufWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Serialize `value` onto `out`. [`BufWriter`] never fails, and a string or
/// JSON object always serializes, so a writer error is dropped.
fn append_json(out: &mut BytesMut, value: &impl Serialize) {
    let _ = serde_json::to_writer(BufWriter(out), value);
}

/// Validate one `APPEND_RESOURCE_OUTPUT` payload into records.
///
/// Newline-separated, trailing newline optional, blank segments skipped;
/// no records at all is [`RecordError::Empty`]. `already_ended` refuses
/// anything after a `session_end`, including later records in the batch.
///
/// # Errors
///
/// The first [`RecordError`]; the caller commits nothing unless `Ok`.
pub fn validate(bytes: &[u8], already_ended: bool) -> Result<Vec<ValidRecord>, RecordError> {
    let text = std::str::from_utf8(bytes).map_err(|_| RecordError::NotUtf8)?;
    let mut ended = already_ended;
    let mut records = Vec::new();
    for line in text.split('\n') {
        // An optional missing final LF is normalized by stamping, so reserve
        // it here as well. Check before trimming: whitespace is still bytes.
        let len = line.len().saturating_add(1);
        if len > MAX_RECORD_BYTES {
            return Err(RecordError::Oversized { len });
        }
        let line = line.trim_matches(|c: char| c == '\r' || c == ' ' || c == '\t');
        if line.is_empty() {
            continue;
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

#[cfg(test)]
mod tests;
