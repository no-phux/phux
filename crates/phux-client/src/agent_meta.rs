//! Typed view of the `phux.agent/v1` L3 metadata record (ADR-0040).
//!
//! The record is the structured agent identity + lifecycle path that
//! replaces title-substring heuristics: an agent (or an integration acting
//! for it) writes this record to the Terminal it runs in via `SET_METADATA`;
//! consumers read it back and MUST prefer it over OSC-title or screen
//! inference ([`docs/spec/L3.md`](../../../docs/spec/L3.md) §3.7). The
//! server stores the bytes opaquely — the schema here is the normative
//! *client* convention, exactly like `phux.tags/v1`.
//!
//! `state` and `attention` are OPEN string enums on the wire: an
//! unrecognized value decodes to [`AgentMetaState::Unknown`] /
//! [`AgentAttention::Normal`] rather than failing the parse, so the
//! vocabulary can grow without breaking older consumers.

use serde::{Deserialize, Serialize};

pub use phux_protocol::wire::frame::RESOURCE_AGENT_KEY;
pub use phux_protocol::wire::frame::RESOURCE_PANE_OCCUPANT_KEY;

/// Server-observed foreground process for `phux.pane-occupant/v1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneOccupantRecord {
    /// Login-dash-stripped basename of the foreground process.
    pub foreground: String,
    /// Whether it is the pane's original interactive shell.
    pub is_pane_shell: bool,
}

/// Parse a `phux.pane-occupant/v1` value. Malformed or future-incompatible
/// records are absent evidence, never an available-shell answer.
#[must_use]
pub fn parse_pane_occupant(bytes: &[u8]) -> Option<PaneOccupantRecord> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if !value.is_object() {
        return None;
    }
    let record: PaneOccupantRecord = serde_json::from_value(value).ok()?;
    if record.foreground.is_empty()
        || record.foreground.chars().any(char::is_control)
        || record.foreground.contains('/')
    {
        return None;
    }
    Some(record)
}

/// Lifecycle state a `phux.agent/v1` record declares.
///
/// OPEN enum: an unrecognized wire string decodes as [`Self::Unknown`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", from = "String")]
pub enum AgentMetaState {
    /// No state declared, or an unrecognized (newer) vocabulary value.
    #[default]
    Unknown,
    /// Available and not actively working.
    Idle,
    /// Actively doing work.
    Working,
    /// Waiting on human input or otherwise blocked.
    Blocked,
    /// Finished its task.
    Done,
}

impl From<String> for AgentMetaState {
    /// OPEN-enum decode: any string not in the v1 vocabulary is `Unknown`.
    fn from(word: String) -> Self {
        match word.as_str() {
            "idle" => Self::Idle,
            "working" => Self::Working,
            "blocked" => Self::Blocked,
            "done" => Self::Done,
            _ => Self::Unknown,
        }
    }
}

impl AgentMetaState {
    /// The kebab-case wire/display word for this state.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
        }
    }
}

/// Attention priority a `phux.agent/v1` record declares.
///
/// OPEN enum: an unrecognized wire string decodes as [`Self::Normal`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", from = "String")]
pub enum AgentAttention {
    /// Explicitly no attention requested.
    None,
    /// Low-priority background signal.
    Low,
    /// Normal priority, and the fallback for unrecognized values.
    #[default]
    Normal,
    /// Should be surfaced prominently.
    High,
}

impl From<String> for AgentAttention {
    /// OPEN-enum decode: any string not in the v1 vocabulary is `Normal`.
    fn from(word: String) -> Self {
        match word.as_str() {
            "none" => Self::None,
            "low" => Self::Low,
            "high" => Self::High,
            _ => Self::Normal,
        }
    }
}

impl AgentAttention {
    /// The kebab-case wire/display word for this attention level.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
        }
    }
}

/// The `phux.agent/v1` record: one agent's declared identity + lifecycle,
/// scoped to the Terminal it runs in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRecord {
    /// Human-facing agent name (REQUIRED, non-empty).
    pub name: String,
    /// Open-vocabulary kind slug, e.g. `"claude"`, `"codex"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Declared lifecycle state; absent means unknown.
    #[serde(default)]
    pub state: AgentMetaState,
    /// Declared attention priority; absent derives from `state`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<AgentAttention>,
    /// Free-form association label (fleet/job name); the terminal
    /// association is the metadata key's Terminal scope itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

impl AgentRecord {
    /// The effective attention: the declared level, or the conventional
    /// derivation from `state` when absent (`blocked` is high, `working`
    /// normal, `done`/`unknown` low, `idle` none).
    #[must_use]
    pub fn effective_attention(&self) -> AgentAttention {
        self.attention.unwrap_or(match self.state {
            AgentMetaState::Blocked => AgentAttention::High,
            AgentMetaState::Working => AgentAttention::Normal,
            AgentMetaState::Done | AgentMetaState::Unknown => AgentAttention::Low,
            AgentMetaState::Idle => AgentAttention::None,
        })
    }

    /// The window/tab label a chrome consumer renders for this record:
    /// `name (state)`, with a `!` prefix when effective attention is high.
    /// Purely structured — no title parsing anywhere.
    #[must_use]
    pub fn label(&self) -> String {
        let bang = if self.effective_attention() == AgentAttention::High {
            "!"
        } else {
            ""
        };
        format!("{bang}{} ({})", self.name, self.state.as_str())
    }

    /// Encode this record to the UTF-8 JSON bytes `SET_METADATA` carries.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
}

/// Compatibility identity heuristic (phux-foz.9): infer an agent name
/// from a pane's OSC 0/2 title when no `phux.agent/v1` record is
/// declared.
///
/// Plain `claude` / `codex` CLI sessions do not write the ADR-0040
/// record (nothing calls `phux agent set` for them), so a consumer that
/// only trusted records would show no agent at all. This is the same
/// built-in token matching `phux agent`'s detector uses for identity
/// (`crates/phux/src/commands/agent/detect.rs`), kept deliberately
/// narrow — case-insensitive substring on the two well-known CLI names,
/// nothing screen-scraped. A declared record MUST still win wherever
/// both exist (L3.md §3.7).
#[must_use]
pub fn agent_name_from_title(title: &str) -> Option<&'static str> {
    let lower = title.to_lowercase();
    if lower.contains("codex") {
        return Some("codex");
    }
    if lower.contains("claude") {
        return Some("claude");
    }
    None
}

/// Decode a `phux.agent/v1` metadata value.
///
/// Returns `None` for bytes that are not a JSON object with a non-empty
/// `name` — the spec'd "no declared agent" reading — so a malformed write
/// can never wedge a consumer.
#[must_use]
pub fn parse_agent_record(bytes: &[u8]) -> Option<AgentRecord> {
    // Route through `Value` so only a JSON *object* is accepted — serde
    // would otherwise happily fill struct fields positionally from a JSON
    // array, which the spec calls malformed.
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if !value.is_object() {
        return None;
    }
    let record: AgentRecord = serde_json::from_value(value).ok()?;
    if record.name.trim().is_empty() {
        return None;
    }
    Some(record)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_a_full_record() {
        let record = AgentRecord {
            name: "reviewer".to_owned(),
            kind: Some("claude".to_owned()),
            state: AgentMetaState::Working,
            attention: Some(AgentAttention::Low),
            session: Some("wave1".to_owned()),
        };
        let parsed = parse_agent_record(&record.encode()).expect("roundtrip");
        assert_eq!(parsed, record);
    }

    #[test]
    fn pane_occupant_is_strict_and_privacy_bounded() {
        let record = parse_pane_occupant(br#"{"foreground":"zsh","is_pane_shell":true}"#)
            .expect("valid occupant");
        assert_eq!(record.foreground, "zsh");
        assert!(record.is_pane_shell);

        for malformed in [
            br#"{"foreground":"","is_pane_shell":true}"#.as_slice(),
            br#"{"foreground":"/bin/zsh","is_pane_shell":true}"#.as_slice(),
            br#"{"foreground":"zsh\n","is_pane_shell":true}"#.as_slice(),
            br#"{"foreground":"zsh"}"#.as_slice(),
            br#"["zsh",true]"#.as_slice(),
        ] {
            assert_eq!(parse_pane_occupant(malformed), None, "{malformed:?}");
        }
    }

    #[test]
    fn minimal_record_defaults_state_and_attention() {
        let parsed = parse_agent_record(br#"{"name":"codex"}"#).expect("parse");
        assert_eq!(parsed.name, "codex");
        assert_eq!(parsed.state, AgentMetaState::Unknown);
        assert_eq!(parsed.attention, None);
        assert_eq!(parsed.effective_attention(), AgentAttention::Low);
    }

    #[test]
    fn unknown_state_and_attention_words_are_tolerated() {
        let parsed =
            parse_agent_record(br#"{"name":"a","state":"hibernating","attention":"maximal"}"#)
                .expect("open enums must not fail the parse");
        assert_eq!(parsed.state, AgentMetaState::Unknown);
        assert_eq!(parsed.attention, Some(AgentAttention::Normal));
    }

    #[test]
    fn rejects_missing_or_empty_name_and_malformed_json() {
        assert_eq!(parse_agent_record(br#"{"state":"idle"}"#), None);
        assert_eq!(parse_agent_record(br#"{"name":"  "}"#), None);
        assert_eq!(parse_agent_record(b"not json"), None);
        assert_eq!(parse_agent_record(br#"["name"]"#), None);
    }

    /// phux-foz.9: the OSC-title compatibility heuristic recognizes the two
    /// well-known CLI names (case-insensitive, embedded in a longer title)
    /// and nothing else.
    #[test]
    fn title_heuristic_matches_known_cli_names_only() {
        assert_eq!(agent_name_from_title("claude"), Some("claude"));
        assert_eq!(agent_name_from_title("Claude Code - ~/src"), Some("claude"));
        assert_eq!(agent_name_from_title("codex resume"), Some("codex"));
        assert_eq!(agent_name_from_title("CODEX"), Some("codex"));
        assert_eq!(agent_name_from_title("vim src/main.rs"), None);
        assert_eq!(agent_name_from_title(""), None);
    }

    #[test]
    fn label_is_structured_and_flags_high_attention() {
        let mut record = AgentRecord {
            name: "reviewer".to_owned(),
            state: AgentMetaState::Blocked,
            ..AgentRecord::default()
        };
        assert_eq!(record.label(), "!reviewer (blocked)");
        record.state = AgentMetaState::Idle;
        assert_eq!(record.label(), "reviewer (idle)");
    }
}
