//! Chrome layer — status bar, dividers, pane borders.
//!
//! Split into submodules so wave-2 work can land in disjoint files:
//! - [`status_bar`] — bottom-row status widget (phux-5ke.2)
//! - [`dividers`] — pane separators and borders (phux-5ke.3)
//! - [`sidebar`] — window/tab sidebar strip (phux-4h5a)
//!
//! The one thing that is NOT split is the badge vocabulary: a pane's
//! state has to read the same in the sidebar row and on the pane's own
//! title, or the two surfaces are describing different machines. See
//! [`agent_badge`].

pub mod dividers;
pub mod sidebar;
pub mod status_bar;

use ratatui::style::Color;

use crate::render::theme::Theme;
use phux_client::agent_meta::AgentMetaState;

/// How one agent's state is drawn, wherever it is drawn.
///
/// Shape carries the state and colour reinforces it — deliberately not
/// colour alone. Four states told apart only by hue are four states a
/// colour-blind reader cannot tell apart at all, and a 1-cell glyph is
/// the whole budget the sidebar and a pane title each have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentBadge {
    /// The single-cell glyph.
    pub glyph: &'static str,
    /// Its theme colour.
    pub color: Color,
    /// `true` when the badge should also be bold: the row is asking for
    /// a human right now, or holding an unread result.
    pub emphatic: bool,
}

/// Filled dot: blocked, or a pane that asked for a human.
pub(crate) const AGENT_BLOCKED_GLYPH: &str = "●";
/// Half-filled ring: actively working, not waiting.
pub(crate) const AGENT_WORKING_GLYPH: &str = "◐";

/// Resolve the badge for one agent pane.
///
/// ```text
/// ● blocked        (or attention: the pane asked a question)
/// ◆ done, unread   ("look at me")
/// ◐ working
/// ○ done+seen / idle / unknown
/// ```
#[must_use]
pub fn agent_badge(
    theme: &Theme,
    state: AgentMetaState,
    attention: bool,
    seen: bool,
) -> AgentBadge {
    let color = match state {
        AgentMetaState::Idle => theme.agent_idle,
        AgentMetaState::Working => theme.agent_working,
        AgentMetaState::Blocked => theme.agent_blocked,
        AgentMetaState::Done => theme.agent_done,
        AgentMetaState::Unknown => theme.dim,
    };
    let unreviewed_done = state == AgentMetaState::Done && !seen;
    let glyph = match state {
        AgentMetaState::Blocked => AGENT_BLOCKED_GLYPH,
        // "look at me": finished, unread.
        AgentMetaState::Done if !seen => "◆",
        AgentMetaState::Working => AGENT_WORKING_GLYPH,
        AgentMetaState::Done | AgentMetaState::Idle | AgentMetaState::Unknown => "○",
    };
    AgentBadge {
        glyph,
        color,
        emphatic: attention || unreviewed_done,
    }
}

/// The badge for a pane that is not running a declared agent but HAS
/// asked for a human (ADR-0035 `AgentEvent::Asked`).
///
/// Same filled dot as `blocked`, in the same attention tone: "a person
/// is being waited on" is one fact, and it must not read as two
/// depending on whether an agent record happened to be declared.
#[must_use]
pub const fn attention_badge(theme: &Theme) -> AgentBadge {
    AgentBadge {
        glyph: AGENT_BLOCKED_GLYPH,
        color: theme.attention,
        emphatic: true,
    }
}
