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
//!
//! The sibling `phux.pane-occupant/v1` record ([`PaneOccupantRecord`]) and
//! the **available-shell precondition** built on it live here too, because
//! the precondition is a reading of that record: every verb that types a
//! shell command line into a pane someone else owns (`phux agent start`,
//! `phux run`) answers the same question — is a shell actually in the
//! foreground? — and must answer it from one implementation, or the two
//! surfaces drift. See [`pane_shell_availability`].

use std::path::Path;
use std::time::Duration;

use phux_core::screen::{ScreenState, SemanticContent};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::Scope;
use serde::{Deserialize, Serialize};

use crate::attach::connection::{Answer, Connection};

pub use phux_protocol::wire::frame::RESOURCE_AGENT_KEY;
pub use phux_protocol::wire::frame::RESOURCE_ASKED_KEY;
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

// ---------------------------------------------------------------------------
// The available-shell precondition.
//
// herdr's gate is three clauses: the pane's foreground pgid equals its child
// pid, the job holds only that shell, and the name is a known shell. phux has
// the raw materials for clauses 1 and 3 server-side, where the ADR-0046
// detector already makes both process queries, and publishes the
// privacy-bounded answer as `phux.pane-occupant/v1`. OSC-133 is a
// conservative cross-check: a Prompt/Input mark on the cursor row
// corroborates availability, while marks elsewhere positively prove the
// screen is busy and override a possibly stale periodic process observation.
// ---------------------------------------------------------------------------

/// How long [`read_pane_occupant`] waits for the record to appear.
///
/// Covers the detector's first 500 ms unidentified tick without turning an
/// older or degraded server into an unbounded preflight.
pub const PANE_OCCUPANT_WAIT: Duration = Duration::from_millis(650);

/// What the client-side available-shell check could establish.
///
/// Three-valued because two of the answers are refusals for different reasons
/// and the third is an admission. Collapsing them would make a verb either
/// unusable (refusing every pane without shell integration) or unsafe
/// (typing into `vim`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellCheck {
    /// An OSC-133 `Prompt`/`Input` mark sits on the cursor's row: whatever
    /// holds the screen right now is a shell command line.
    AtPrompt,
    /// The pane carries OSC-133 marks somewhere, but not on the cursor's row.
    /// Shell integration is on and the cursor is somewhere else — positive
    /// evidence that something other than the prompt has the screen.
    NotAtPrompt,
    /// No semantic marks at all, or no resolvable cursor. Shell integration is
    /// probably off, and phux cannot answer the question client-side.
    Unanswerable,
}

impl ShellCheck {
    /// The wire word for `--json`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AtPrompt => "at-prompt",
            Self::NotAtPrompt => "not-at-prompt",
            Self::Unanswerable => "unanswerable",
        }
    }
}

/// Evaluate the available-shell precondition against one screen.
///
/// Deliberately scoped to the **cursor's row** rather than the whole viewport:
/// a prompt mark left further up the screen is equally true while a build runs
/// in the foreground, and that is exactly the case the precondition exists to
/// catch.
#[must_use]
pub fn shell_check(screen: &ScreenState) -> ShellCheck {
    let Some(cells) = screen.cells.as_ref() else {
        return ShellCheck::Unanswerable;
    };
    let marked: Vec<&phux_core::screen::CellInfo> = cells
        .iter()
        .filter(|cell| {
            matches!(
                cell.semantic,
                Some(SemanticContent::Input | SemanticContent::Prompt)
            )
        })
        .collect();
    if marked.is_empty() {
        return ShellCheck::Unanswerable;
    }
    let Some(cursor) = screen.cursor.as_ref() else {
        return ShellCheck::Unanswerable;
    };
    if marked.iter().any(|cell| cell.row == cursor.y) {
        ShellCheck::AtPrompt
    } else {
        ShellCheck::NotAtPrompt
    }
}

/// Whether a pane may be typed a shell command line, and why not when not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellAvailability {
    /// A shell is in the foreground; the command line is safe to submit.
    Available,
    /// The server observed a foreground process that is not the pane shell.
    /// Carries the login-dash-stripped basename, so a caller can name what
    /// IS in the foreground.
    BusyProcess(String),
    /// OSC-133 marks prove something other than the prompt has the screen.
    BusyScreen,
    /// Neither source answered: no occupant record and no marks. Fail
    /// CLOSED — an unevaluable precondition is not one that passed.
    Unanswerable,
}

/// Combine the server-owned occupant record with the client-visible OSC-133
/// reading.
///
/// A missing screen cannot erase a positive server observation, but it is
/// never itself evidence of safety.
#[must_use]
pub fn shell_availability(
    occupant: Option<&PaneOccupantRecord>,
    screen: ShellCheck,
) -> ShellAvailability {
    if let Some(occupant) = occupant
        && !occupant.is_pane_shell
    {
        return ShellAvailability::BusyProcess(occupant.foreground.clone());
    }
    match screen {
        ShellCheck::AtPrompt => ShellAvailability::Available,
        ShellCheck::NotAtPrompt => ShellAvailability::BusyScreen,
        ShellCheck::Unanswerable if occupant.is_some() => ShellAvailability::Available,
        ShellCheck::Unanswerable => ShellAvailability::Unanswerable,
    }
}

/// The available-shell precondition for `terminal`: server process truth,
/// conservatively cross-checked with client-visible OSC-133 state.
///
/// The one implementation behind `phux agent start`'s precondition and
/// `phux run`'s. Both reads are side-effect-free (a `GET_METADATA` and a
/// `GET_SCREEN`), so evaluating the precondition never disturbs the pane it
/// is asking about.
///
/// Fails CLOSED when neither source answers — see
/// [`ShellAvailability::Unanswerable`].
pub async fn pane_shell_availability(socket: &Path, terminal: &ResourceId) -> ShellAvailability {
    let occupant = read_pane_occupant(socket, terminal).await;
    let screen = crate::snapshot::get_screen_scrollback(socket, terminal.clone(), None, true).await;
    let screen = screen
        .as_ref()
        .map_or(ShellCheck::Unanswerable, shell_check);
    shell_availability(occupant.as_ref(), screen)
}

/// Read the detector-owned occupant record, allowing its first 500 ms tick
/// to land.
///
/// Absence remains distinguishable from `is_pane_shell: false` so the
/// OSC-133 compatibility fallback can serve older or degraded servers.
pub async fn read_pane_occupant(
    socket: &Path,
    terminal: &ResourceId,
) -> Option<PaneOccupantRecord> {
    let mut conn = Connection::connect(socket).await.ok()?;
    let deadline = tokio::time::Instant::now() + PANE_OCCUPANT_WAIT;
    let mut request_id = 70;
    loop {
        let reply = conn
            .request_metadata(
                request_id,
                Scope::Resource(terminal.clone()),
                RESOURCE_PANE_OCCUPANT_KEY.to_owned(),
            )
            .await
            .ok()?;
        let (answer, _) = reply.into_parts();
        match answer {
            Answer::Ok(Some(bytes)) => {
                drop(conn);
                return parse_pane_occupant(&bytes);
            }
            Answer::Ok(None) => {}
            Answer::Err(_) => {
                drop(conn);
                return None;
            }
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            drop(conn);
            return None;
        }
        tokio::time::sleep(crate::wait::DEFAULT_POLL_INTERVAL.min(remaining)).await;
        request_id += 1;
    }
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

    /// Encode this record to the UTF-8 JSON bytes `SET_METADATA` carries.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
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
    use phux_core::screen::{CellInfo, CellStyle, CursorState};

    use super::*;

    fn cell(row: u16, col: u16, semantic: Option<SemanticContent>) -> CellInfo {
        CellInfo {
            col,
            row,
            semantic,
            style: CellStyle::default(),
        }
    }

    fn cursor_at(y: u16) -> CursorState {
        CursorState {
            x: 2,
            y,
            visible: true,
        }
    }

    fn marked_screen(cursor: Option<CursorState>, cells: Option<Vec<CellInfo>>) -> ScreenState {
        ScreenState {
            pane: 7,
            cols: 80,
            rows: 24,
            cursor,
            lines: vec![String::new(); 24],
            cells,
            ..ScreenState::default()
        }
    }

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

    /// The precondition reads the CURSOR'S row, not the whole viewport: a
    /// prompt mark left further up the screen is equally true while a build
    /// runs in the foreground, which is the case this check exists to catch.
    #[test]
    fn the_shell_check_reads_the_cursor_row_not_the_whole_screen() {
        let at_prompt = marked_screen(
            Some(cursor_at(10)),
            Some(vec![cell(10, 0, Some(SemanticContent::Prompt))]),
        );
        assert_eq!(shell_check(&at_prompt), ShellCheck::AtPrompt);

        // Same marks, cursor elsewhere: something else has the screen.
        let busy = marked_screen(
            Some(cursor_at(10)),
            Some(vec![cell(3, 0, Some(SemanticContent::Prompt))]),
        );
        assert_eq!(shell_check(&busy), ShellCheck::NotAtPrompt);
    }

    #[test]
    fn server_shell_evidence_fills_only_the_unanswerable_case() {
        let shell = PaneOccupantRecord {
            foreground: "zsh".to_owned(),
            is_pane_shell: true,
        };
        let busy = PaneOccupantRecord {
            foreground: "vim".to_owned(),
            is_pane_shell: false,
        };
        assert_eq!(
            shell_availability(Some(&shell), ShellCheck::Unanswerable),
            ShellAvailability::Available
        );
        assert_eq!(
            shell_availability(Some(&shell), ShellCheck::NotAtPrompt),
            ShellAvailability::BusyScreen
        );
        assert_eq!(
            shell_availability(Some(&busy), ShellCheck::AtPrompt),
            ShellAvailability::BusyProcess("vim".to_owned())
        );
        assert_eq!(
            shell_availability(None, ShellCheck::Unanswerable),
            ShellAvailability::Unanswerable
        );
    }

    /// No marks at all is an ADMISSION, not a refusal reason of the same
    /// kind: shell integration is probably off and phux cannot answer. The
    /// two must stay distinguishable, because only one of them is evidence.
    #[test]
    fn a_screen_without_semantic_marks_is_unanswerable() {
        assert_eq!(
            shell_check(&marked_screen(Some(cursor_at(0)), None)),
            ShellCheck::Unanswerable
        );
        assert_eq!(
            shell_check(&marked_screen(Some(cursor_at(0)), Some(Vec::new()))),
            ShellCheck::Unanswerable
        );
        // Styled cells with no semantic mark carry no prompt evidence either.
        assert_eq!(
            shell_check(&marked_screen(
                Some(cursor_at(0)),
                Some(vec![cell(0, 0, None)])
            )),
            ShellCheck::Unanswerable
        );
        // Marks but no resolvable cursor: the question has no anchor.
        assert_eq!(
            shell_check(&marked_screen(
                None,
                Some(vec![cell(0, 0, Some(SemanticContent::Input))])
            )),
            ShellCheck::Unanswerable
        );
    }
}
