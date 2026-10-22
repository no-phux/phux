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
    AgentAttention, AgentState, AttentionReading, MAX_RAW_WORD_BYTES, StateReading,
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
    use super::{
        AgentAttention, AgentBadge, AgentState, AttentionReading, MAX_RAW_WORD_BYTES, StateReading,
        badge,
    };
    use phux_protocol::ResourceId;

    fn read(record: &str) -> AgentBadge {
        badge(&ResourceId::local(1), Some(record.as_bytes()))
    }

    #[test]
    fn open_vocabulary_state_keeps_the_defaults() {
        assert_eq!(
            badge(
                &ResourceId::local(9),
                Some(br#"{"name":"Claude","kind":"claude","state":"newer"}"#),
            ),
            AgentBadge {
                terminal_id: ResourceId::local(9),
                name: "Claude".to_owned(),
                kind: Some("claude".to_owned()),
                session: None,
                state: AgentState::Unknown,
                attention: AgentAttention::Low,
                state_reading: StateReading::Unsupported("newer".to_owned()),
                attention_reading: AttentionReading::Derived,
            }
        );
    }

    #[test]
    fn malformed_records_clear_the_badge() {
        assert_eq!(
            badge(&ResourceId::local(2), Some(b"not-json")),
            AgentBadge {
                terminal_id: ResourceId::local(2),
                name: String::new(),
                kind: None,
                session: None,
                state: AgentState::Unknown,
                attention: AgentAttention::Low,
                state_reading: StateReading::NoRecord,
                attention_reading: AttentionReading::Derived,
            }
        );
        for record in [r#"["blocked"]"#, r#"{"state":"blocked"}"#, r#"{"name":7}"#] {
            assert_eq!(
                read(record).state_reading,
                StateReading::NoRecord,
                "{record}"
            );
        }
    }

    #[test]
    fn a_tombstone_clears_the_badge() {
        let cleared = badge(&ResourceId::local(3), None);
        assert!(cleared.name.is_empty());
        assert_eq!(cleared.state, AgentState::Unknown);
        assert_eq!(cleared.attention, AgentAttention::Low);
        assert_eq!(cleared.state_reading, StateReading::NoRecord);
        assert_eq!(cleared.attention_reading, AttentionReading::Derived);
    }

    #[test]
    fn a_nameless_record_clears_the_badge() {
        let cleared = badge(
            &ResourceId::local(4),
            Some(br#"{"name":"","state":"working"}"#),
        );
        assert!(cleared.name.is_empty());
        assert_eq!(cleared.state, AgentState::Unknown);
        assert_eq!(cleared.state_reading, StateReading::NoRecord);
    }

    #[test]
    fn a_whitespace_only_name_clears_the_badge() {
        let cleared = badge(
            &ResourceId::local(4),
            Some(br#"{"name":"   ","state":"blocked"}"#),
        );
        assert!(cleared.name.is_empty());
        assert_eq!(cleared.state, AgentState::Unknown);
        assert_eq!(cleared.attention, AgentAttention::Low);
        assert_eq!(cleared.state_reading, StateReading::NoRecord);
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
    fn the_session_label_is_carried() {
        assert_eq!(
            badge(
                &ResourceId::local(8),
                Some(br#"{"name":"a","session":"rung-a"}"#),
            )
            .session,
            Some("rung-a".to_owned())
        );
    }

    #[test]
    fn absent_explicit_unknown_and_unsupported_states_stay_distinct() {
        // All four normalize to Unknown (L3 §3.7); only the reading differs.
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
    fn blocked_followed_by_a_newer_word_is_unsupported_not_cleared() {
        let blocked = read(r#"{"name":"a","kind":"claude","state":"blocked"}"#);
        assert_eq!(blocked.attention, AgentAttention::High);
        let newer = read(r#"{"name":"a","kind":"claude","state":"waiting_approval"}"#);
        // Still a declared agent, still asking to be noticed: never an
        // all-clear, and distinguishable from the record going away.
        assert_eq!(newer.name, "a");
        assert_eq!(newer.state, AgentState::Unknown);
        assert_ne!(newer.attention, AgentAttention::None);
        assert_eq!(
            newer.state_reading,
            StateReading::Unsupported("waiting_approval".to_owned())
        );
        assert_ne!(
            newer.state_reading,
            badge(&ResourceId::local(1), None).state_reading
        );
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
