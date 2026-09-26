//! The `phux.agent/v1` L3 record, folded into a badge (ADR-0040/0046).
//!
//! `state` and `attention` are OPEN vocabularies on the wire: a newer word
//! must degrade, never fail. An absent record, a tombstone, malformed bytes,
//! and a record without a name all fold to the same thing — "no declared
//! agent" — so a stale badge can never survive a pane's change of hands.
//!
//! When the record declares no `attention`, the effective level is derived
//! from `state` exactly as `docs/spec/L3.md` §3.7 specifies.

use phux_protocol::ResourceId;

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
}

/// Fold `bytes` — a `phux.agent/v1` value, or its absence — into a badge.
#[must_use]
pub fn badge(terminal_id: &ResourceId, bytes: Option<&[u8]>) -> AgentBadge {
    let record = bytes.and_then(parse).unwrap_or_default();
    let attention = record
        .attention
        .unwrap_or_else(|| derived_attention(record.state));
    AgentBadge {
        terminal_id: terminal_id.clone(),
        name: record.name,
        kind: record.kind,
        session: record.session,
        state: record.state,
        attention,
    }
}

#[derive(Default)]
struct Record {
    name: String,
    kind: Option<String>,
    session: Option<String>,
    state: AgentState,
    attention: Option<AgentAttention>,
}

fn parse(bytes: &[u8]) -> Option<Record> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object()?;
    let name = object.get("name")?.as_str()?;
    if name.is_empty() {
        return None;
    }
    Some(Record {
        name: name.to_owned(),
        kind: object
            .get("kind")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        session: object
            .get("session")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        state: object
            .get("state")
            .and_then(|value| value.as_str())
            .map_or(AgentState::Unknown, state_from),
        attention: object
            .get("attention")
            .and_then(|value| value.as_str())
            .map(attention_from),
    })
}

fn state_from(word: &str) -> AgentState {
    match word {
        "idle" => AgentState::Idle,
        "working" => AgentState::Working,
        "blocked" => AgentState::Blocked,
        "done" => AgentState::Done,
        _ => AgentState::Unknown,
    }
}

fn attention_from(word: &str) -> AgentAttention {
    match word {
        "none" => AgentAttention::None,
        "low" => AgentAttention::Low,
        "high" => AgentAttention::High,
        _ => AgentAttention::Normal,
    }
}

const fn derived_attention(state: AgentState) -> AgentAttention {
    match state {
        AgentState::Blocked => AgentAttention::High,
        AgentState::Working => AgentAttention::Normal,
        AgentState::Done | AgentState::Unknown => AgentAttention::Low,
        AgentState::Idle => AgentAttention::None,
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentAttention, AgentBadge, AgentState, badge};
    use phux_protocol::ResourceId;

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
            }
        );
    }

    #[test]
    fn a_tombstone_clears_the_badge() {
        let cleared = badge(&ResourceId::local(3), None);
        assert!(cleared.name.is_empty());
        assert_eq!(cleared.state, AgentState::Unknown);
        assert_eq!(cleared.attention, AgentAttention::Low);
    }

    #[test]
    fn a_nameless_record_clears_the_badge() {
        let cleared = badge(
            &ResourceId::local(4),
            Some(br#"{"name":"","state":"working"}"#),
        );
        assert!(cleared.name.is_empty());
        assert_eq!(cleared.state, AgentState::Unknown);
    }

    #[test]
    fn attention_is_derived_from_state_when_undeclared() {
        for (state, wanted) in [
            ("idle", AgentAttention::None),
            ("working", AgentAttention::Normal),
            ("blocked", AgentAttention::High),
            ("done", AgentAttention::Low),
        ] {
            let record = format!(r#"{{"name":"a","state":"{state}"}}"#);
            assert_eq!(
                badge(&ResourceId::local(5), Some(record.as_bytes())).attention,
                wanted,
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
    }

    #[test]
    fn an_unrecognized_attention_reads_as_normal() {
        assert_eq!(
            badge(
                &ResourceId::local(7),
                Some(br#"{"name":"a","attention":"screaming"}"#),
            )
            .attention,
            AgentAttention::Normal
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
}
