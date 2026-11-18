//! The server's copy of the `phux.agent/v1` record shape (ADR-0040,
//! `docs/agents.md#agent-states`), duplicated from `phux-client` (no dependency).
//!
//! `metadata_set` dedups on byte equality, so field order (`name`, `kind`,
//! `state`, `attention`, `session`) and the skip set are pinned by tests.

use serde::{Deserialize, Serialize};

/// The `phux.agent/v1` record. `state` is an open-vocabulary word, always
/// emitted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRecordJson {
    /// Human-facing agent name. REQUIRED and non-empty per the spec.
    pub name: String,
    /// Open-vocabulary kind slug, e.g. `"claude"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Lifecycle word: `unknown` | `idle` | `working` | `blocked` | `done`.
    #[serde(default)]
    pub state: String,
    /// Never set by the detector (derived from `state` when absent);
    /// preserved when a human declared it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
    /// Free-form association label (fleet / job name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

impl AgentRecordJson {
    /// Decode a stored record; `None` for anything not a JSON object.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
        if !value.is_object() {
            return None;
        }
        serde_json::from_value(value).ok()
    }

    /// Encode to the UTF-8 JSON bytes `SET_METADATA` carries.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::AgentRecordJson;

    /// Re-encoding a decoded record is stable — the property the dedup
    /// actually relies on.
    #[test]
    fn encode_decode_roundtrips_byte_for_byte() {
        let bytes = br#"{"name":"claude","kind":"claude","state":"idle","session":"fleet-1"}"#;
        let record = AgentRecordJson::decode(bytes).expect("decodes");
        assert_eq!(record.encode(), bytes.to_vec());
    }

    #[test]
    fn optional_fields_are_skipped_when_absent() {
        let record = AgentRecordJson {
            name: "codex".to_owned(),
            kind: None,
            state: "idle".to_owned(),
            attention: None,
            session: None,
        };
        assert_eq!(
            String::from_utf8(record.encode()).expect("utf8"),
            r#"{"name":"codex","state":"idle"}"#
        );
    }

    #[test]
    fn non_object_json_is_not_a_record() {
        assert!(AgentRecordJson::decode(b"[1,2,3]").is_none());
        assert!(AgentRecordJson::decode(b"\"hello\"").is_none());
        assert!(AgentRecordJson::decode(b"not json").is_none());
    }
}

#[cfg(test)]
mod frozen {
    #[test]
    fn agent_record() {
        phux_frozen::json::<super::AgentRecordJson>("agent-record").check();
    }
}
