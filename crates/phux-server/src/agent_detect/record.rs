//! The server's copy of the `phux.agent/v1` record shape (ADR-0040,
//! `docs/spec/L3.md` §3.7), duplicated from `phux-client` (no dependency).
//! `metadata_set` dedups on byte equality, so field order (`name`, `kind`,
//! `state`, `attention`, `session`) and the skip set are pinned by a golden
//! test.

use serde::{Deserialize, Serialize};

/// The `phux.agent/v1` record. `state` is an open-vocabulary word, always
/// emitted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AgentRecordJson {
    /// Human-facing agent name. REQUIRED and non-empty per the spec.
    pub(crate) name: String,
    /// Open-vocabulary kind slug, e.g. `"claude"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<String>,
    /// Lifecycle word: `unknown` | `idle` | `working` | `blocked` | `done`.
    #[serde(default)]
    pub(crate) state: String,
    /// Never set by the detector (derived from `state` when absent);
    /// preserved when a human declared it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) attention: Option<String>,
    /// Free-form association label (fleet / job name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<String>,
}

impl AgentRecordJson {
    /// Decode a stored record; `None` for anything not a JSON object.
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
        if !value.is_object() {
            return None;
        }
        serde_json::from_value(value).ok()
    }

    /// Encode to the UTF-8 JSON bytes `SET_METADATA` carries.
    pub(crate) fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::AgentRecordJson;

    /// GOLDEN: the exact bytes the detector writes (dedup depends on them).
    #[test]
    fn golden_encoding_is_byte_exact() {
        let record = AgentRecordJson {
            name: "claude".to_owned(),
            kind: Some("claude".to_owned()),
            state: "working".to_owned(),
            attention: None,
            session: None,
        };
        assert_eq!(
            String::from_utf8(record.encode()).expect("utf8"),
            r#"{"name":"claude","kind":"claude","state":"working"}"#
        );
    }

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
    fn a_declared_attention_survives_a_roundtrip() {
        let bytes = br#"{"name":"a","state":"blocked","attention":"high"}"#;
        let record = AgentRecordJson::decode(bytes).expect("decodes");
        assert_eq!(record.attention.as_deref(), Some("high"));
    }

    #[test]
    fn non_object_json_is_not_a_record() {
        assert!(AgentRecordJson::decode(b"[1,2,3]").is_none());
        assert!(AgentRecordJson::decode(b"\"hello\"").is_none());
        assert!(AgentRecordJson::decode(b"not json").is_none());
    }
}
