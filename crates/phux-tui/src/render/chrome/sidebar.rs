//! Fixed-area Agents and Sessions sidebar painter.
//!
//! Agents lists every agent in caller-supplied stable order, narrowed by a
//! one-click filter row under its header (every agent, this session's, or
//! only the ones that need you, crossed with a machine; see
//! [`agent_filter`]), and keeps a fixed share (`[sidebar] agents-share`) of
//! the body whatever its population; an empty list paints a quiet
//! placeholder.
//! Sessions fill the rest, with a host line
//! per machine when more than one is known, the active session expanding its
//! tabs beneath it. Plugin sections (ADR-0148) sit between them at fixed
//! heights. The last
//! row holds New tab with a collapse chevron in the corner; a rule on the
//! pane-facing column separates the strip from the panes. [`hit_test`] reads
//! the same row model as the painter, and an unchanged repaint emits nothing.

use crate::render::chrome::sidebar_sections::PluginShape;
use phux_client::agent_meta::AgentMetaState;
use phux_protocol::ids::{ResourceId, SessionId};

pub mod agent_filter;
mod projects;
pub use agent_filter::{AgentFilter, AgentScope, FilterChip, FilterRequest, HostFilter, Step};

mod compose;
mod layout;
mod lines;
mod painter;
mod rows;
use compose::justify;
pub use layout::{hit_test, row_model};
use layout::{roster_segment, segment_starts};
pub use painter::{SidebarPainter, satellite_open_action};
pub use rows::{
    SessionRosterTarget, SidebarHit, SidebarRow, SidebarRule, SidebarTarget, SidebarTargets,
};

/// Label of the "create" affordance row (runs `new-window`).
pub const NEW_LABEL: &str = "+ new tab";
/// Agents header.
pub const NEEDS_YOU_HEADER: &str = "Agents";
/// Quiet placeholder under the `need` filter when no agent needs a human.
///
/// Short enough for a 20-column strip, and free of the word "agent" so a
/// row search by agent name cannot match it.
pub const AGENTS_EMPTY_LABEL: &str = "none need you";
/// The same placeholder under any other filter: nothing it admits is running.
pub const AGENTS_NONE_LABEL: &str = "none running";
/// Sessions header, including the current session.
pub const SPACES_HEADER: &str = "Sessions";
/// Quiet placeholder under an empty plugin section.
pub const PLUGIN_EMPTY: &str = "none";
/// The collapse chevron painted in the strip's bottom corner.
/// Clicking it runs `toggle-sidebar`.
pub const COLLAPSE_GLYPH: &str = "‹";

/// Minimum strip height (rows) at which the footer affordances render.
/// Below this every row goes to the section body — a 2–3 row strip
/// showing only chrome and no windows would be useless.
const MIN_FOOTER_HEIGHT: u16 = 4;

/// Whole-cell spacing tokens, shared by every sidebar row (docs/experience.md).
const GUTTER: u16 = 1;
const ICON_COLUMNS: usize = 2;

/// One agent-running pane as the Agents area renders it (from an ADR-0040
/// record or an `AgentSession` stream).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEntry {
    /// The session holding the agent's pane, `None` for ours. `None` commits
    /// `select-window` (client-local focus); `Some` commits a real
    /// `switch-session { name, window, pane }` re-attach.
    pub session: Option<String>,
    /// Stable identity of [`Self::session`] when the row came from the
    /// session graph. `None` for the attached session (`session` is also
    /// `None`) and for satellite hops whose ids are not attachable here.
    pub session_id: Option<SessionId>,
    /// Stable pane identity when the row came from a peer layout or the
    /// server graph. `None` for a local row, which commits `select-window`.
    pub resource: Option<ResourceId>,
    /// Index of the window holding the agent's pane (its `select-window`
    /// index) — clicking the row jumps there.
    pub window: usize,
    /// The window's stored name, herdr's "workspace" column on the row.
    pub window_name: String,
    /// The pane's DFS leaf ordinal, for a cross-session commit to land on the
    /// pane rather than its window's remembered focus.
    pub pane: Option<usize>,
    /// Agent display name, e.g. `claude` or `merge-queue-w5`.
    pub name: String,
    /// Lifecycle state; picks the row's glyph + color.
    pub state: AgentMetaState,
    /// `true` when the agent is waiting on a human (declared high
    /// attention, or the pane's ADR-0035 asked flag).
    pub attention: bool,
    /// Satellite host this agent is running on (ADR-0136). `None` for an
    /// agent on the attached server. Painted in the chord color, the same
    /// tone as a satellite pane's border badge.
    pub host: Option<String>,
    /// The user visited the pane since its last state change: a `done` agent
    /// not yet looked at reads "look at me". Part of the cache key; the
    /// change timestamp deliberately is not (it would repaint every frame).
    pub seen: bool,
}

impl AgentEntry {
    /// Whether this agent needs a human: blocked or asking, or finished and
    /// not yet looked at. Only these rows reach the Agents list.
    #[must_use]
    pub const fn needs_human(&self) -> bool {
        attention_rank(self.state, self.attention, self.seen) >= 3
    }
}

/// An agent's rung on the attention ladder (higher wants a human sooner);
/// drives badges and session summaries, not display order.
///
/// ```text
/// blocked  >  done AND !seen  >  working  >  done/idle AND seen  >  unknown
/// ```
///
/// `attention` (declared high attention, or an ADR-0035 ask) pins the top
/// rung: an agent that asked for a human is blocked on one.
#[must_use]
pub const fn attention_rank(state: AgentMetaState, attention: bool, seen: bool) -> u8 {
    if attention {
        return 4;
    }
    match state {
        AgentMetaState::Blocked => 4,
        AgentMetaState::Done if !seen => 3,
        AgentMetaState::Working => 2,
        AgentMetaState::Done | AgentMetaState::Idle => 1,
        AgentMetaState::Unknown => 0,
    }
}

/// One session with its serving host (an unreachable host may add an
/// unselectable placeholder). Counts, not one worst colour: a dot says what,
/// a count says how much.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent row facts (current, selectable, satellite, linked), not a state machine"
)]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionRosterEntry {
    /// The session's name — the human-facing label a click still carries.
    pub name: String,
    /// Stable session identity for local rows. `None` for satellite
    /// placeholders: those ids are host-local and not attachable here.
    pub id: Option<SessionId>,
    /// Display label of the serving host, supplied by the projection.
    pub host: String,
    /// Whether this is the session currently attached to the client.
    pub active: bool,
    /// Satellite name for the `switch-session` host argument, if required.
    pub route_host: Option<String>,
    /// ADR-0140: the machine (`phux.hosts/v1` row name) a click re-attaches
    /// to with `switch-host`, for a session on another machine. `None` for
    /// the attached server's sessions and a hub's satellites.
    pub switch_host: Option<String>,
    /// False for unreachable-host placeholders; actual sessions are selectable.
    pub selectable: bool,
    /// Panes on the top rung: blocked, or explicitly asking for a human.
    pub blocked: usize,
    /// Panes running work right now.
    pub working: usize,
    /// Panes that finished while the user was elsewhere and have not been
    /// visited since. The rung that makes this roster worth reading.
    pub done_unvisited: usize,
    /// Panes that are idle, or done and already reviewed.
    pub settled: usize,
    /// Panes whose agent state could not be determined. Always the count for
    /// a satellite session, whose per-Terminal metadata this client may not
    /// subscribe to (`docs/spec/L3.md` §5).
    pub unknown: usize,
    /// `true` for a session on a federated satellite. Its state is
    /// structurally unknowable from here, so the row is painted as explicitly
    /// unknown rather than being allowed to read as a calm zero.
    pub satellite: bool,
    /// ADR-0168: the project group this row sits in, the same value on every
    /// member row; `None` outside a group.
    pub group: Option<RosterGroup>,
    /// A linked worktree, drawn one step further in than its primary
    /// checkout.
    pub linked: bool,
}

/// A project group in the Sessions zone (ADR-0168), carried by each of its
/// member rows so the header can paint from the first one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RosterGroup {
    /// The grouping key, which a header click toggles.
    pub key: String,
    /// The project's display name.
    pub name: String,
    /// Whether the user collapsed the group.
    pub collapsed: bool,
    /// Panes across the group's sessions that need you (blocked or asking).
    pub needs_you: usize,
    /// Member sessions the collapsed group is not showing.
    pub hidden: usize,
}

impl SessionRosterEntry {
    /// The highest rung any of the session's panes occupies (same rungs as
    /// [`attention_rank`]).
    #[must_use]
    pub const fn top_rank(&self) -> u8 {
        if self.blocked > 0 {
            4
        } else if self.done_unvisited > 0 {
            3
        } else if self.working > 0 {
            2
        } else if self.settled > 0 {
            1
        } else {
            0
        }
    }

    /// Total panes counted into this row, across every rung.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.blocked + self.working + self.done_unvisited + self.settled + self.unknown
    }
}

/// The counts the strip's shape derives from, so the dispatcher can
/// hit-test without rebuilding the lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SidebarCounts {
    /// Agents that need a human, in stable input order.
    pub needs_you: usize,
    /// Windows in the focused session, nested beneath its roster entry.
    pub windows: usize,
    /// All session entries, including the current session.
    pub roster: usize,
    /// Roster index whose windows expand, or None when there is no active entry.
    pub active_session: Option<usize>,
    /// ADR-0140: bit `j` is set when roster entry `j` opens a machine
    /// segment (its host differs from entry `j - 1`'s), so a host header row
    /// precedes it. A bitmask keeps the shape `Copy`; entries past the 128th
    /// open no segment, which a strip never has the rows to show anyway.
    pub host_starts: u128,
    /// ADR-0168: bit `j` is set when the segment opening at roster entry `j`
    /// is a project group, whose header toggles it rather than switching.
    pub project_starts: u128,
    /// Which column carries the separator rule. Part of the shape because
    /// it moves every hit target: the rule and its collapse chevron are not
    /// row targets, and the rows shift away from a leading rule.
    pub rule: SidebarRule,
    /// ADR-0148: the plugin sections' reserved heights and populations.
    pub plugin: PluginShape,
    /// Percent of the body the Agents panel keeps (`[sidebar] agents-share`),
    /// whatever its population; `0` drops the panel. Carried here so a click
    /// resolves against the same fixed boundary the painter laid.
    pub agents_share: u8,
    /// Whether the filter row carries the `@` machine chip: the agents span
    /// more than one machine, or a machine filter is set.
    pub agent_host_chip: bool,
}

/// Shipped `[sidebar] agents-share` (ADR-0112: half the body), mirrored so
/// the row allocator's default and the schema default agree.
pub const DEFAULT_AGENTS_SHARE: u8 = 50;

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests;
