//! The `phux.agent/v1` L3 record, folded into a badge (ADR-0040/0046).
//!
//! `state` and `attention` are OPEN vocabularies on the wire: a newer word
//! must degrade, never fail. An absent record, a tombstone, malformed bytes,
//! and a record without a name all fold to the same thing — "no declared
//! agent" — so a stale badge can never survive a pane's change of hands.
//!
//! When the record declares no `attention`, the effective level is derived
//! from `state` exactly as `docs/agents.md#agent-states` specifies.
//!
//! The normalized `state` and `attention` are what L3 says they mean, and a
//! consumer's gates read them. Alongside them a badge carries how each was
//! *read* ([`StateReading`], [`AttentionReading`]): no record at all, a
//! record that omitted the field, an explicit `"unknown"`, a recognized word,
//! or a word this build does not know, kept as bounded raw text. That is
//! provenance, not a second vocabulary: it lets a consumer show a newer
//! producer's `"waiting_approval"` as present-but-unsupported rather than
//! fold it into "no agent" or an all-clear.

use phux_client_core::agent_record::parse_agent_record;
pub use phux_client_core::agent_record::{
    AgentAttention, AgentState, AttentionReading, StateReading,
};
use phux_protocol::ResourceId;

/// One pane's effective agent badge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentBadge {
    /// The pane.
    pub terminal_id: ResourceId,
    /// Human-facing agent name; empty when no record is declared.
    pub name: String,
    /// Open-vocabulary kind slug (for example `claude`), when declared.
    pub kind: Option<String>,
    /// Free-form association label the record declared.
    pub session: Option<String>,
    /// The declared lifecycle word.
    pub state: AgentState,
    /// The EFFECTIVE attention: declared, or derived from `state`.
    pub attention: AgentAttention,
    /// How `state` was read.
    pub state_reading: StateReading,
    /// How `attention` was arrived at.
    pub attention_reading: AttentionReading,
}

/// Fold `bytes` — a `phux.agent/v1` value, or its absence — into a badge.
/// The record is read by `phux_client_core::agent_record`, the one reader
/// every client shares.
#[must_use]
pub fn badge(terminal_id: &ResourceId, bytes: Option<&[u8]>) -> AgentBadge {
    let record = bytes.and_then(parse_agent_record).unwrap_or_default();
    let (attention, attention_reading) = record.effective_attention();
    AgentBadge {
        terminal_id: terminal_id.clone(),
        name: record.name,
        kind: record.kind,
        session: record.session,
        state: record.state,
        attention,
        state_reading: record.state_reading,
        attention_reading,
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentAttention, AgentBadge, AgentState, AttentionReading, StateReading, badge};
    use phux_client_core::agent_record::MAX_RAW_WORD_BYTES;
    use phux_protocol::ResourceId;

    fn read(record: &str) -> AgentBadge {
        badge(&ResourceId::local(1), Some(record.as_bytes()))
    }

    /// A tombstone, an empty name and a whitespace-only name each clear the
    /// badge to the same no-record reading.
    #[test]
    fn a_tombstone_or_an_unusable_name_clears_the_badge() {
        let cases: [(&str, Option<&[u8]>); 3] = [
            ("tombstone", None),
            ("nameless", Some(br#"{"name":"","state":"working"}"#)),
            (
                "whitespace name",
                Some(br#"{"name":"   ","state":"blocked"}"#),
            ),
        ];
        for (label, record) in cases {
            let cleared = badge(&ResourceId::local(3), record);
            assert!(cleared.name.is_empty(), "{label}");
            assert_eq!(cleared.state, AgentState::Unknown, "{label}");
            assert_eq!(cleared.attention, AgentAttention::Low, "{label}");
            assert_eq!(cleared.state_reading, StateReading::NoRecord, "{label}");
            assert_eq!(
                cleared.attention_reading,
                AttentionReading::Derived,
                "{label}"
            );
        }
    }

    #[test]
    fn attention_is_derived_from_state_when_undeclared() {
        for (state, wanted) in [
            ("idle", AgentAttention::None),
            ("working", AgentAttention::Normal),
            ("blocked", AgentAttention::High),
            ("done", AgentAttention::Low),
            ("unknown", AgentAttention::Low),
            ("waiting_approval", AgentAttention::Low),
        ] {
            let derived = read(&format!(r#"{{"name":"a","state":"{state}"}}"#));
            assert_eq!(derived.attention, wanted, "state {state}");
            assert_eq!(
                derived.attention_reading,
                AttentionReading::Derived,
                "state {state}"
            );
        }
    }

    #[test]
    fn a_declared_attention_wins_over_the_derivation() {
        let declared = badge(
            &ResourceId::local(6),
            Some(br#"{"name":"a","state":"blocked","attention":"none"}"#),
        );
        assert_eq!(declared.state, AgentState::Blocked);
        assert_eq!(declared.attention, AgentAttention::None);
        assert_eq!(declared.attention_reading, AttentionReading::Declared);
        let normal = read(r#"{"name":"a","attention":"normal"}"#);
        assert_eq!(normal.attention, AgentAttention::Normal);
        assert_eq!(normal.attention_reading, AttentionReading::Declared);
    }

    #[test]
    fn an_unrecognized_attention_reads_as_normal_and_keeps_its_word() {
        let screaming = badge(
            &ResourceId::local(7),
            Some(br#"{"name":"a","attention":"screaming"}"#),
        );
        assert_eq!(screaming.attention, AgentAttention::Normal);
        assert_eq!(
            screaming.attention_reading,
            AttentionReading::Unsupported("screaming".to_owned())
        );
        let numeric = read(r#"{"name":"a","state":"blocked","attention":3}"#);
        assert_eq!(numeric.attention, AgentAttention::Normal);
        assert_eq!(
            numeric.attention_reading,
            AttentionReading::Unsupported("3".to_owned())
        );
    }

    #[test]
    fn absent_explicit_unknown_and_unsupported_states_stay_distinct() {
        // All four normalize to Unknown; only the reading differs.
        let readings = [
            (None, StateReading::NoRecord),
            (Some(r#"{"name":"a"}"#), StateReading::Undeclared),
            (
                Some(r#"{"name":"a","state":"unknown"}"#),
                StateReading::Indeterminate,
            ),
            (
                Some(r#"{"name":"a","state":"waiting_approval"}"#),
                StateReading::Unsupported("waiting_approval".to_owned()),
            ),
        ];
        for (record, wanted) in readings {
            let read = badge(&ResourceId::local(1), record.map(str::as_bytes));
            assert_eq!(read.state, AgentState::Unknown, "{record:?}");
            assert_eq!(read.state_reading, wanted, "{record:?}");
        }
        for word in ["idle", "working", "blocked", "done"] {
            let recognized = read(&format!(r#"{{"name":"a","state":"{word}"}}"#));
            assert_eq!(recognized.state_reading, StateReading::Recognized, "{word}");
        }
    }

    #[test]
    fn malformed_state_values_are_unsupported_and_bounded() {
        for (value, wanted) in [
            ("7", "7"),
            ("null", "null"),
            (r#"{"x":1}"#, r#"{"x":1}"#),
            (r#""Blocked""#, "Blocked"),
            (r#""""#, ""),
            (r#""wait\ning""#, "wait\u{FFFD}ing"),
        ] {
            let read = read(&format!(r#"{{"name":"a","state":{value}}}"#));
            assert_eq!(read.state, AgentState::Unknown, "{value}");
            assert_eq!(
                read.state_reading,
                StateReading::Unsupported(wanted.to_owned()),
                "{value}"
            );
        }
        let long = format!(
            r#"{{"name":"a","state":"{}é"}}"#,
            "x".repeat(MAX_RAW_WORD_BYTES - 1)
        );
        let StateReading::Unsupported(raw) = read(&long).state_reading else {
            panic!("a long word is unsupported");
        };
        // The two-byte character would cross the bound, so it is cut whole.
        assert_eq!(raw, "x".repeat(MAX_RAW_WORD_BYTES - 1));
    }
}
