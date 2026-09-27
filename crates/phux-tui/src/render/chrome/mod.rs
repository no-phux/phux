//! Chrome layer: status bar, dividers, sidebar. The badge vocabulary is
//! shared ([`agent_badge`]) so a pane reads the same on its title and its
//! sidebar row.

pub mod dividers;
pub mod sidebar;
pub mod status_bar;

use ratatui::style::Color;

use crate::render::theme::Theme;
use phux_client::agent_meta::AgentMetaState;

/// How one agent's state is drawn anywhere: shape carries the state and
/// colour reinforces it, so a colour-blind reader can still tell them apart.
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
/// Filled diamond: finished, and nobody has looked yet.
pub(crate) const AGENT_DONE_GLYPH: &str = "◆";

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
        AgentMetaState::Done if !seen => AGENT_DONE_GLYPH,
        AgentMetaState::Working => AGENT_WORKING_GLYPH,
        AgentMetaState::Done | AgentMetaState::Idle | AgentMetaState::Unknown => "○",
    };
    AgentBadge {
        glyph,
        color,
        emphatic: attention || unreviewed_done,
    }
}

/// The badge for a non-agent pane that asked for a human (ADR-0035): the
/// same filled dot as `blocked`.
#[must_use]
pub const fn attention_badge(theme: &Theme) -> AgentBadge {
    AgentBadge {
        glyph: AGENT_BLOCKED_GLYPH,
        color: theme.attention,
        emphatic: true,
    }
}
