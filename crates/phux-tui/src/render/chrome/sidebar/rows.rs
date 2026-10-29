//! The row, hit, and click-target vocabulary the row model and painter share.

use phux_protocol::ids::{ResourceId, SessionId};

use super::SidebarCounts;
use super::agent_filter::FilterChip;
#[cfg(doc)]
use super::{agent_filter, hit_test};

/// The side of the strip that carries the separator rule: always the side
/// that faces the panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SidebarRule {
    /// The last column, for a left-docked strip.
    #[default]
    Trailing,
    /// The first column, for a right-docked strip.
    Leading,
}

impl SidebarRule {
    /// The strip-local column the rule occupies in a `width`-wide strip.
    #[must_use]
    pub const fn column(self, width: u16) -> u16 {
        match self {
            Self::Trailing => width.saturating_sub(1),
            Self::Leading => 0,
        }
    }

    /// The strip-local column where row content (gutter included) starts.
    pub(super) const fn content_x(self) -> u16 {
        match self {
            Self::Trailing => 0,
            Self::Leading => 1,
        }
    }
}

/// One row of the strip, top to bottom. Both the painter and [`hit_test`]
/// derive from this single model, so paint and click targets cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarRow {
    /// Fixed Agents header (legacy name).
    NeedsYouHeader,
    /// The filter chips under the Agents header ([`agent_filter`]).
    AgentFilter,
    /// Agent entry `j`'s row (glyph + session/window + `state - name`).
    NeedsYou(usize),
    /// Quiet placeholder under an Agents panel whose population is empty.
    NeedsYouEmpty,
    /// Agents overflow opens the fleet dashboard.
    NeedsYouOverflow,
    /// Window `i`'s name row.
    WindowName(usize),
    /// Fixed Sessions header (legacy name).
    SpacesHeader,
    /// Roster entry `j`'s row (dot + session name + state histogram).
    RosterEntry(usize),
    /// ADR-0140: the machine segment header opening at roster entry `j`
    /// (host label, and `here` for the attached machine); shares entry
    /// `j`'s target. ADR-0168: a project group's header when the segment
    /// is one; it toggles the group.
    RosterHost(usize),
    /// Hidden sessions/windows; opens the session picker.
    RosterOverflow,
    /// Plugin section `s`'s header.
    PluginHeader(usize),
    /// Plugin section `s`'s row `j`.
    PluginEntry(usize, usize),
    /// Quiet placeholder under an empty plugin section.
    PluginEmpty(usize),
    /// Plugin section `s`'s hidden-row count (inert).
    PluginOverflow(usize),
    /// Unused padding (section gap, or fill above the footer).
    Blank,
    /// The `+ new` affordance (create a window).
    NewWindow,
}

/// The target a mouse position resolves to: index-based (`Copy`, derivable
/// from counts), resolved against [`SidebarTargets`] at commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarHit {
    /// A nested window row selects window `i` (its `select-window` index).
    Window(usize),
    /// Agent row `j` (legacy variant name).
    NeedsYou(usize),
    /// A chip of the Agents filter row.
    AgentFilter(FilterChip),
    /// Session name or host row `j`.
    Roster(usize),
    /// ADR-0168: the project group header opening at roster entry `j`.
    Project(usize),
    /// Plugin section `s`'s row `j`: focuses that row's pane.
    Plugin(usize, usize),
    /// Agents overflow opens the agent-fleet dashboard.
    Fleet,
    /// Sessions overflow opens the session picker.
    Sessions,
    /// The `+ new` affordance.
    NewWindow,
    /// The collapse chevron in the bottom corner —
    /// clicking runs `toggle-sidebar`.
    Collapse,
}

/// What an agent row commits when clicked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarTarget {
    /// A pane in the focused session: move client-local focus, nothing more.
    Window(usize),
    /// A pane in another session: re-attach, select the window, focus the
    /// pane.
    Session {
        /// The peer session's name (display / typed-name fallback).
        name: String,
        /// Stable identity when the row was projected from the session graph.
        id: Option<SessionId>,
        /// Window index within that session. Present only for a persisted
        /// TUI layout; graph-only rows omit it so a click cannot fabricate
        /// a TUI index.
        window: Option<usize>,
        /// Pane ordinal within that window. Present only for a layout leaf.
        pane: Option<usize>,
        /// Server graph / layout leaf the click must land on.
        resource: Option<ResourceId>,
    },
}

/// The click-resolution table for one painted frame.
///
/// Membership can change
/// after the snapshot, so a `switch-session` commits the resolved session
/// id (name as a fallback) rather than re-deriving it from an index.
#[derive(Debug, Clone, Default)]
pub struct SidebarTargets {
    /// The counts the frame was painted from — the same ones [`hit_test`]
    /// must be given, so a click resolves against the shape it landed on.
    pub counts: SidebarCounts,
    /// Agent targets, in stable display order.
    pub needs_you: Vec<SidebarTarget>,
    /// Session destinations in display order; placeholders have no target.
    pub roster: Vec<Option<SessionRosterTarget>>,
    /// ADR-0168: each roster entry's project group key, when it has one.
    pub projects: Vec<Option<String>>,
    /// ADR-0148: per plugin section, each row's `(window, pane)` for
    /// `focus-pane`.
    pub plugin: Vec<Vec<(usize, usize)>>,
}

/// Explicit session identity and optional satellite route for a roster click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRosterTarget {
    /// Session name, resolved against the painted frame.
    pub name: String,
    /// Stable session identity when the row is a local-graph session.
    pub id: Option<SessionId>,
    /// Satellite name passed to `switch-session`, if required.
    pub host: Option<String>,
    /// ADR-0140: the machine a `switch-host` re-attaches to, for a session
    /// on another machine. Wins over [`Self::host`].
    pub switch_host: Option<String>,
}
