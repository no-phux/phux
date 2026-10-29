//! The one `phux.agent/v1` record reader (L3 §3.7, ADR-0040/0046).
//!
//! Every client surface that reads the record calls [`parse_agent_record`]
//! and adapts the result to its own types, so "is there a declared agent,
//! and what does it say" has a single answer across frontends.
//!
//! - A value that is not a JSON object, or whose `name` is absent, not a
//!   string, empty, or only whitespace, is "no declared agent". The writers
//!   (`phux agent set`, the MCP agent tools) refuse a whitespace-only name, so
//!   a reader that accepted one would show a badge with no visible label.
//! - `state` and `attention` are OPEN vocabularies: a newer word, or a value
//!   that is not a string, degrades to `unknown` / `normal` and never fails
//!   the parse. The reading records how each was read, so a consumer can show
//!   a newer producer's word as present-but-unsupported.
//! - `kind` and `session` that are not strings are ignored, not fatal.
//! - A value over [`MAX_AGENT_RECORD_BYTES`] is "no declared agent": any
//!   local client can write the key, so its size is untrusted input.

/// The lifecycle word a record declares.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentState {
    /// No state declared, or an unrecognized (newer) vocabulary value.
    #[default]
    Unknown,
    /// Declared `idle`.
    Idle,
    /// Declared `working`.
    Working,
    /// Declared `blocked`.
    Blocked,
    /// Declared `done`.
    Done,
}

/// How loudly a record asks to be noticed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentAttention {
    /// Declared `none`.
    None,
    /// Declared `low`, or derived from a finished or undeclared state.
    Low,
    /// Declared `normal`, an unrecognized word, or derived from `working`.
    Normal,
    /// Declared `high`, or derived from `blocked`.
    High,
}

/// The most bytes an encoded `phux.agent/v1` record may hold. Writers refuse
/// a larger record, the server ignores a `SET_METADATA` carrying one, and
/// [`parse_agent_record`] reads one as "no declared agent".
pub const MAX_AGENT_RECORD_BYTES: usize = 4096;

/// The most bytes of an unsupported word a reading carries. A record may hold
/// up to [`MAX_AGENT_RECORD_BYTES`]; a vocabulary word is a slug, and a
/// consumer only displays it.
pub const MAX_RAW_WORD_BYTES: usize = 64;

/// How a record's `state` was read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum StateReading {
    /// No record: the key is absent or deleted, or the value is malformed
    /// or nameless. The pane declares no agent.
    #[default]
    NoRecord,
    /// A record that declared no `state`.
    Undeclared,
    /// A record that declared `"unknown"` explicitly.
    Indeterminate,
    /// A word this build recognizes; `state` is that word.
    Recognized,
    /// A word this build does not recognize, or a value that is not a
    /// string: `state` is `Unknown` as L3 requires, and this is the raw
    /// text (bounded to [`MAX_RAW_WORD_BYTES`], control characters
    /// replaced).
    Unsupported(String),
}

/// How a record's effective `attention` was arrived at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AttentionReading {
    /// Undeclared (or no record): derived from `state`.
    #[default]
    Derived,
    /// A word this build recognizes, declared by the record.
    Declared,
    /// A word this build does not recognize, or a value that is not a
    /// string: `attention` is `Normal` as L3 requires, and this is the raw
    /// text, bounded like [`StateReading::Unsupported`].
    Unsupported(String),
}

/// A parsed `phux.agent/v1` record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentRecordReading {
    /// Human-facing agent name, as written; never empty or whitespace-only.
    pub name: String,
    /// Open-vocabulary kind slug, when declared as a string.
    pub kind: Option<String>,
    /// Free-form association label, when declared as a string.
    pub session: Option<String>,
    /// The declared lifecycle word, normalized.
    pub state: AgentState,
    /// How `state` was read.
    pub state_reading: StateReading,
    /// The declared attention and how it was read; `None` when the record
    /// declared no `attention` (derive it with [`derived_attention`]).
    pub attention: Option<(AgentAttention, AttentionReading)>,
}

impl AgentRecordReading {
    /// The effective attention: declared, or derived from `state`.
    #[must_use]
    pub fn effective_attention(&self) -> (AgentAttention, AttentionReading) {
        self.attention
            .clone()
            .unwrap_or_else(|| (derived_attention(self.state), AttentionReading::Derived))
    }
}

/// Decode a `phux.agent/v1` value, or `None` for "no declared agent".
#[must_use]
pub fn parse_agent_record(bytes: &[u8]) -> Option<AgentRecordReading> {
    if bytes.len() > MAX_AGENT_RECORD_BYTES {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object()?;
    let name = object.get("name")?.as_str()?;
    if name.trim().is_empty() {
        return None;
    }
    let (state, state_reading) = object
        .get("state")
        .map_or((AgentState::Unknown, StateReading::Undeclared), state_from);
    Some(AgentRecordReading {
        name: name.to_owned(),
        kind: object
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        session: object
            .get("session")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        state,
        state_reading,
        attention: object.get("attention").map(attention_from),
    })
}

/// The conventional attention for an undeclared `attention` (L3 §3.7):
/// `blocked` is high, `working` normal, `done`/`unknown` low, `idle` none.
#[must_use]
pub const fn derived_attention(state: AgentState) -> AgentAttention {
    match state {
        AgentState::Blocked => AgentAttention::High,
        AgentState::Working => AgentAttention::Normal,
        AgentState::Done | AgentState::Unknown => AgentAttention::Low,
        AgentState::Idle => AgentAttention::None,
    }
}

fn state_from(value: &serde_json::Value) -> (AgentState, StateReading) {
    let state = match value.as_str() {
        Some("unknown") => return (AgentState::Unknown, StateReading::Indeterminate),
        Some("idle") => AgentState::Idle,
        Some("working") => AgentState::Working,
        Some("blocked") => AgentState::Blocked,
        Some("done") => AgentState::Done,
        _ => return (AgentState::Unknown, StateReading::Unsupported(raw(value))),
    };
    (state, StateReading::Recognized)
}

fn attention_from(value: &serde_json::Value) -> (AgentAttention, AttentionReading) {
    let attention = match value.as_str() {
        Some("none") => AgentAttention::None,
        Some("low") => AgentAttention::Low,
        Some("normal") => AgentAttention::Normal,
        Some("high") => AgentAttention::High,
        _ => {
            return (
                AgentAttention::Normal,
                AttentionReading::Unsupported(raw(value)),
            );
        }
    };
    (attention, AttentionReading::Declared)
}

/// The displayable text of an unsupported value: a string as written, any
/// other JSON value as its compact encoding; control characters replaced and
/// cut to [`MAX_RAW_WORD_BYTES`] on a character boundary.
fn raw(value: &serde_json::Value) -> String {
    let text = value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned);
    let mut bounded = String::new();
    for character in text.chars() {
        let character = if character.is_control() {
            char::REPLACEMENT_CHARACTER
        } else {
            character
        };
        if bounded.len() + character.len_utf8() > MAX_RAW_WORD_BYTES {
            break;
        }
        bounded.push(character);
    }
    bounded
}

#[cfg(test)]
mod tests {
    use super::{AgentState, MAX_AGENT_RECORD_BYTES, StateReading, parse_agent_record};

    /// The record is untrusted input: one byte over the limit is no
    /// declared agent, however well-formed; exactly at the limit still reads.
    #[test]
    fn a_record_over_the_byte_limit_is_no_declared_agent() {
        let record_of = |len: usize| {
            let frame = br#"{"name":"a","session":""}"#.len();
            format!(r#"{{"name":"a","session":"{}"}}"#, "x".repeat(len - frame))
        };
        let at = record_of(MAX_AGENT_RECORD_BYTES);
        assert_eq!(at.len(), MAX_AGENT_RECORD_BYTES);
        assert!(parse_agent_record(at.as_bytes()).is_some());
        let over = record_of(MAX_AGENT_RECORD_BYTES + 1);
        assert_eq!(parse_agent_record(over.as_bytes()), None);
    }

    #[test]
    fn an_empty_or_whitespace_only_name_is_no_declared_agent() {
        for record in [
            r#"{"name":""}"#,
            r#"{"name":"  "}"#,
            r#"{"name":"\t\n","state":"working"}"#,
            r#"{"state":"idle"}"#,
            r#"{"name":7}"#,
            r#"["name"]"#,
            "not json",
        ] {
            assert_eq!(parse_agent_record(record.as_bytes()), None, "{record}");
        }
    }

    #[test]
    fn a_padded_name_is_kept_as_written() {
        let read = parse_agent_record(br#"{"name":" a ","state":"done"}"#).expect("record");
        assert_eq!(read.name, " a ");
        assert_eq!(read.state, AgentState::Done);
        assert_eq!(read.state_reading, StateReading::Recognized);
    }

    #[test]
    fn non_string_optional_fields_degrade_without_failing_the_parse() {
        let read = parse_agent_record(br#"{"name":"a","kind":3,"session":[],"state":4}"#)
            .expect("an open field never fails the parse");
        assert_eq!(read.kind, None);
        assert_eq!(read.session, None);
        assert_eq!(read.state, AgentState::Unknown);
        assert_eq!(
            read.state_reading,
            StateReading::Unsupported("4".to_owned())
        );
    }
}
