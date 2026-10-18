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

use std::io::{self, Write};

use phux_config::widget::WindowInfo;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect as RataRect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::layout::Rect;
use crate::render::Theme;
use crate::render::chrome::sidebar_sections::{PluginSection, PluginSectionSpec, PluginShape};
use crate::render::chrome::{AGENT_BLOCKED_GLYPH as AGENT_BLOCKED, AGENT_DONE_GLYPH as AGENT_DONE};
use crate::render::{clip_text, display_width};
use phux_client::agent_meta::AgentMetaState;
use phux_protocol::ids::{ResourceId, SessionId};

pub mod agent_filter;
mod projects;
pub use agent_filter::{AgentFilter, AgentScope, FilterChip, FilterRequest, HostFilter, Step};
use projects::{foreign_member_host, roster_indent};

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
    const fn content_x(self) -> u16 {
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

/// The row model: a fixed Agents band over Sessions (see the `agents_budget`
/// helper), plugin sections between them; name/host pairs never split. A
/// one-row strip keeps session navigation.
#[must_use]
pub fn row_model(counts: SidebarCounts, h: u16) -> Vec<SidebarRow> {
    let h = usize::from(h);
    let show_footer = h >= usize::from(MIN_FOOTER_HEIGHT);
    let body = h - usize::from(show_footer);
    let mut rows = Vec::with_capacity(h);

    if body == 1 {
        rows.push(SidebarRow::RosterOverflow);
    } else {
        // Plugin sections claim their fixed heights first, and only while
        // Agents and Sessions keep their minimum; population never decides.
        let sections = counts.plugin.fitting(body);
        let core = body - counts.plugin.height(sections);
        // ADR-0112: while the strip has anything to show, the Agents panel
        // keeps a fixed share of the body whatever its population, so gaining
        // or resolving an agent never moves Sessions. A strip with no content
        // at all paints no panels.
        let agents = if counts.needs_you > 0 || counts.roster > 0 {
            agents_budget(counts.agents_share, core)
        } else {
            0
        };
        push_agents(&mut rows, counts.needs_you, agents);
        rows.resize(agents, SidebarRow::Blank);
        push_plugin_sections(&mut rows, counts.plugin, sections);
        push_sessions(&mut rows, counts, core - agents);
        rows.resize(body, SidebarRow::Blank);
    }
    if show_footer {
        rows.push(SidebarRow::NewWindow);
    }
    rows
}

/// The Agents panel's height: `share` percent of the body rows left after the
/// plugin bands, rounded to nearest, kept to `1..core` so both panels keep at
/// least a header. Depends only on the viewport, the plugin bands, and the
/// share — never on the population (ADR-0112).
#[must_use]
fn agents_budget(share: u8, core: usize) -> usize {
    if share == 0 || core < 2 {
        return 0;
    }
    let share = usize::from(share);
    let want = (core * share + 50) / 100;
    want.clamp(1, core - 1)
}

/// Keep overflow reachable even when only one content row fits. An empty
/// population paints a quiet placeholder, not a blank panel (ADR-0112).
fn push_agents(rows: &mut Vec<SidebarRow>, count: usize, budget: usize) {
    if budget == 0 {
        return;
    }
    rows.push(SidebarRow::NeedsYouHeader);
    let mut room = budget - 1;
    if room == 0 {
        return;
    }
    // The filter row only when a list row still fits under it.
    if room >= 2 {
        rows.push(SidebarRow::AgentFilter);
        room -= 1;
    }
    if count == 0 {
        rows.push(SidebarRow::NeedsYouEmpty);
        return;
    }
    let overflow = count > room;
    let shown = count.min(room - usize::from(overflow));
    rows.extend((0..shown).map(SidebarRow::NeedsYou));
    if overflow {
        rows.push(SidebarRow::NeedsYouOverflow);
    }
}

/// Append the first `n` plugin sections, each a header plus exactly its
/// reserved rows: entries, an empty placeholder, or entries with a `+N` last
/// row when the population overflows.
fn push_plugin_sections(rows: &mut Vec<SidebarRow>, shape: PluginShape, n: usize) {
    for (s, (reserved, count)) in shape.sections().take(n).enumerate() {
        let end = rows.len() + 1 + usize::from(reserved);
        rows.push(SidebarRow::PluginHeader(s));
        let room = usize::from(reserved);
        if count == 0 {
            rows.push(SidebarRow::PluginEmpty(s));
        } else if count > room {
            rows.extend((0..room - 1).map(|j| SidebarRow::PluginEntry(s, j)));
            rows.push(SidebarRow::PluginOverflow(s));
        } else {
            rows.extend((0..count).map(|j| SidebarRow::PluginEntry(s, j)));
        }
        rows.resize(end, SidebarRow::Blank);
    }
}

/// Append the fixed Sessions area with complete host pairs and nested windows.
fn push_sessions(rows: &mut Vec<SidebarRow>, counts: SidebarCounts, budget: usize) {
    if budget == 0 || counts.roster == 0 {
        return;
    }
    let end = rows.len() + budget;
    rows.push(SidebarRow::SpacesHeader);
    let room = budget - 1;
    if room == 0 {
        return;
    }
    let want = session_row_count(counts);
    let reserve = want > room;
    let limit = end - usize::from(reserve);
    let start = rows.len();
    push_session_entries(rows, counts, limit);
    if rows.len() - start < want && rows.len() < end {
        rows.push(SidebarRow::RosterOverflow);
    }
}

fn session_row_count(counts: SidebarCounts) -> usize {
    let windows = counts
        .active_session
        .filter(|i| *i < counts.roster)
        .map_or(0, |_| counts.windows);
    let headers = (0..counts.roster.min(128))
        .filter(|j| opens_segment(counts, *j))
        .count();
    counts
        .roster
        .saturating_add(headers)
        .saturating_add(windows)
}

/// Whether roster entry `j` opens a machine segment (see
/// [`SidebarCounts::host_starts`]).
const fn opens_segment(counts: SidebarCounts, j: usize) -> bool {
    j < 128 && counts.host_starts & (1u128 << j) != 0
}

/// The [`SidebarCounts::host_starts`] mask for `segments`, in roster order:
/// a segment is a project group (`true`, its key) or a host (`false`, its
/// label). One machine opens no segment: a lone host header says nothing.
fn segment_starts<'a>(segments: impl Iterator<Item = (bool, &'a str)>) -> u128 {
    let mut mask = 0u128;
    let mut previous: Option<(bool, &str)> = None;
    for (j, segment) in segments.take(128).enumerate() {
        if previous != Some(segment) {
            mask |= 1u128 << j;
        }
        previous = Some(segment);
    }
    if mask.count_ones() < 2 { 0 } else { mask }
}

/// The roster segment entry `s` belongs to.
fn roster_segment(s: &SessionRosterEntry) -> (bool, &str) {
    s.group
        .as_ref()
        .map_or((false, s.host.as_str()), |group| (true, group.key.as_str()))
}

/// Limit iteration by viewport capacity, even for extremely large counts.
/// A segment header and its first session are indivisible: a header never
/// paints without a session under it.
fn push_session_entries(rows: &mut Vec<SidebarRow>, counts: SidebarCounts, limit: usize) {
    for j in 0..counts.roster {
        let header = opens_segment(counts, j);
        if limit.saturating_sub(rows.len()) < 1 + usize::from(header) {
            break;
        }
        if header {
            rows.push(SidebarRow::RosterHost(j));
        }
        rows.push(SidebarRow::RosterEntry(j));
        if counts.active_session == Some(j) {
            let shown = counts.windows.min(limit - rows.len());
            rows.extend((0..shown).map(SidebarRow::WindowName));
        }
    }
}

/// Resolve an outer-viewport cell to a sidebar target; `None` off the strip,
/// on the rule, or on a blank row.
///
/// Headers open their management views; the
/// rule's bottom corner is the collapse chevron. `counts` must match the
/// painted frame.
#[must_use]
pub fn hit_test(rect: Rect, counts: SidebarCounts, x: u16, y: u16) -> Option<SidebarHit> {
    let local_x = x.checked_sub(rect.x)?;
    let local_y = y.checked_sub(rect.y)?;
    if local_x >= rect.w || local_y >= rect.h {
        return None;
    }
    let rule_x = counts.rule.column(rect.w);
    // The rule's bottom cell is the collapse chevron whenever the footer
    // renders (same condition the painter uses).
    if collapse_visible(rect) && local_x == rule_x && local_y == rect.h - 1 {
        return Some(SidebarHit::Collapse);
    }
    // The rest of the rule column is not a target.
    if local_x == rule_x {
        return None;
    }
    let row = *row_model(counts, rect.h).get(usize::from(local_y))?;
    if row == SidebarRow::AgentFilter {
        let col = local_x.checked_sub(counts.rule.content_x() + GUTTER)?;
        return agent_filter::filter_chips(text_width(rect.w), counts.agent_host_chip)
            .into_iter()
            .find(|(start, w, _)| col >= *start && col < start + w)
            .map(|(_, _, chip)| SidebarHit::AgentFilter(chip));
    }
    if let SidebarRow::RosterHost(j) = row
        && j < 128
        && counts.project_starts & (1u128 << j) != 0
    {
        return Some(SidebarHit::Project(j));
    }
    row_hit(row)
}

/// The text area's width inside the gutters and the separator rule.
const fn text_width(strip_w: u16) -> u16 {
    strip_w.saturating_sub(1 + GUTTER * 2)
}

const fn collapse_visible(rect: Rect) -> bool {
    rect.h >= MIN_FOOTER_HEIGHT && rect.w >= 2
}

const fn row_hit(row: SidebarRow) -> Option<SidebarHit> {
    match row {
        SidebarRow::WindowName(i) => Some(SidebarHit::Window(i)),
        SidebarRow::NeedsYou(j) => Some(SidebarHit::NeedsYou(j)),
        SidebarRow::RosterEntry(j) | SidebarRow::RosterHost(j) => Some(SidebarHit::Roster(j)),
        SidebarRow::NeedsYouHeader | SidebarRow::NeedsYouOverflow => Some(SidebarHit::Fleet),
        SidebarRow::SpacesHeader | SidebarRow::RosterOverflow => Some(SidebarHit::Sessions),
        SidebarRow::NewWindow => Some(SidebarHit::NewWindow),
        SidebarRow::PluginEntry(s, j) => Some(SidebarHit::Plugin(s, j)),
        SidebarRow::Blank
        | SidebarRow::AgentFilter
        | SidebarRow::NeedsYouEmpty
        | SidebarRow::PluginHeader(_)
        | SidebarRow::PluginEmpty(_)
        | SidebarRow::PluginOverflow(_) => None,
    }
}

/// Compact branch context is optional; never displace the window identity.
fn fitting_branch(window: &WindowInfo, remaining: usize) -> Option<&str> {
    window
        .branch
        .as_deref()
        .filter(|branch| !branch.is_empty() && display_width(branch) < remaining)
}

/// VT painter for the window sidebar.
#[derive(Debug)]
pub struct SidebarPainter {
    windows: Vec<WindowInfo>,
    /// Every agent the projection supplied, before the filter.
    agents: Vec<AgentEntry>,
    /// What the Agents panel admits.
    agent_filter: AgentFilter,
    /// The agents the filter admits: the rows the panel lists.
    needs_you: Vec<AgentEntry>,
    roster: Vec<SessionRosterEntry>,
    /// ADR-0148: the declared plugin sections, and their last projection.
    plugin_specs: Vec<PluginSectionSpec>,
    plugin: Vec<PluginSection>,
    theme: Theme,
    /// Last successfully emitted cells. Keep only one projection copy and
    /// compare rendered rows so hidden/truncated changes emit no bytes.
    last: Option<(Rect, Buffer)>,
    dirty: bool,
    rule: SidebarRule,
    /// `[sidebar] agents-share`: percent of the body the Agents area
    /// reserves while any agent needs a human; `0` drops it.
    agents_share: u8,
    /// Window index under a live row drag, painted as the insertion
    /// marker. `None` when no window drag is over this strip.
    drop_at: Option<usize>,
    /// Strip rows where the pane grid's rules (the rail and every horizontal
    /// divider reaching this side) meet the separator rule, drawn as tees so
    /// the two read as one frame. Empty when no rule meets it.
    junctions: Vec<u16>,
}

/// Open a satellite agent beside the focused pane. `None` when `resource`
/// is local: that click stays a session switch.
#[must_use]
pub fn satellite_open_action(
    resource: &ResourceId,
) -> Option<phux_config::keybind::ResolvedAction> {
    resource.host()?;
    let mut args = std::collections::BTreeMap::new();
    args.insert(
        "direction".to_owned(),
        toml::Value::String("horizontal".to_owned()),
    );
    args.insert(
        "resource".to_owned(),
        toml::Value::String(phux_client::selector::format_terminal_id(resource)),
    );
    Some(phux_config::keybind::ResolvedAction {
        action: "split-pane".to_owned(),
        args,
    })
}

impl SidebarPainter {
    /// A painter styled by `theme`, initially showing no windows.
    #[must_use]
    pub const fn new(theme: Theme) -> Self {
        Self {
            windows: Vec::new(),
            agents: Vec::new(),
            agent_filter: AgentFilter {
                scope: AgentScope::All,
                host: HostFilter::Any,
            },
            needs_you: Vec::new(),
            roster: Vec::new(),
            plugin_specs: Vec::new(),
            plugin: Vec::new(),
            theme,
            last: None,
            dirty: true,
            rule: SidebarRule::Trailing,
            agents_share: DEFAULT_AGENTS_SHARE,
            drop_at: None,
            junctions: Vec::new(),
        }
    }

    /// The theme this strip paints with; other chrome resolves badges
    /// through it so every surface shares one palette.
    #[must_use]
    pub const fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Put the separator rule on the pane-facing side. Painters call this
    /// with the reservation's edge before each paint, so the click targets
    /// snapshotted from [`Self::click_targets`] match the frame on screen.
    pub fn set_rule(&mut self, rule: SidebarRule) {
        if self.rule != rule {
            self.rule = rule;
            self.dirty = true;
        }
    }

    /// Tee the separator rule into the pane grid at strip `rows` (empty
    /// clears them). Painters call this with the rail row and the rows of the
    /// horizontal dividers that run into the strip, beside [`Self::set_rule`].
    pub fn set_junctions(&mut self, rows: &[u16]) {
        if self.junctions != rows {
            self.junctions = rows.to_vec();
            self.dirty = true;
        }
    }

    /// Replace the window list; true when it changed (callers without another
    /// paint trigger gate a repaint on it).
    pub fn set_windows(&mut self, windows: Vec<WindowInfo>) -> bool {
        if self.windows == windows {
            return false;
        }
        self.windows = windows;
        self.dirty = true;
        true
    }

    /// Replace the agent list (every agent, in stable order); the panel
    /// lists the ones the [filter](Self::set_agent_filter) admits. Same
    /// change-report contract as [`Self::set_windows`].
    pub fn set_agents(&mut self, agents: Vec<AgentEntry>) -> bool {
        if self.agents == agents {
            return false;
        }
        self.agents = agents;
        self.refilter();
        true
    }

    /// The Agents panel's filter.
    #[must_use]
    pub const fn agent_filter(&self) -> &AgentFilter {
        &self.agent_filter
    }

    /// The satellite machines among the agents, for stepping the `@` chip.
    #[must_use]
    pub fn agent_hosts(&self) -> Vec<String> {
        agent_filter::agent_hosts(&self.agents)
    }

    /// Narrow the Agents panel to `filter`. Same change-report contract as
    /// [`Self::set_windows`].
    pub fn set_agent_filter(&mut self, filter: AgentFilter) -> bool {
        if self.agent_filter == filter {
            return false;
        }
        self.agent_filter = filter;
        self.refilter();
        true
    }

    /// Consuming form of [`Self::set_agent_filter`] for construction sites.
    #[must_use]
    pub fn with_agent_filter(mut self, filter: AgentFilter) -> Self {
        self.agent_filter = filter;
        self
    }

    /// Recompute the listed rows from every agent and the filter. Always
    /// repaints: the chip row's shape follows the machine set, which can
    /// move while the listed rows stay the same.
    fn refilter(&mut self) {
        self.needs_you = self
            .agents
            .iter()
            .filter(|entry| self.agent_filter.admits(entry))
            .cloned()
            .collect();
        self.dirty = true;
    }

    /// Replace the Sessions roster, including the current session. Same change-report
    /// contract as [`Self::set_windows`].
    pub fn set_roster(&mut self, roster: Vec<SessionRosterEntry>) -> bool {
        if self.roster == roster {
            return false;
        }
        self.roster = roster;
        self.dirty = true;
        true
    }

    /// Declare the plugin sections (ADR-0148) the strip lays out; their rows
    /// arrive through [`Self::set_plugin_sections`].
    #[must_use]
    pub fn with_plugin_specs(mut self, specs: Vec<PluginSectionSpec>) -> Self {
        self.plugin_specs = specs;
        self
    }

    /// The declared plugin sections, for the projection that fills them.
    #[must_use]
    pub fn plugin_specs(&self) -> &[PluginSectionSpec] {
        &self.plugin_specs
    }

    /// Set the `[sidebar] agents-share` percent the row allocator reads. Same
    /// change-report contract as [`Self::set_windows`]: true when it moved, so
    /// a caller without another paint trigger can repaint on it.
    pub const fn set_agents_share(&mut self, agents_share: u8) -> bool {
        if self.agents_share == agents_share {
            return false;
        }
        self.agents_share = agents_share;
        self.dirty = true;
        true
    }

    /// Consuming form of [`Self::set_agents_share`] for construction sites.
    #[must_use]
    pub const fn with_agents_share(mut self, agents_share: u8) -> Self {
        self.agents_share = agents_share;
        self
    }

    /// Replace the plugin sections' projected rows. Same change-report
    /// contract as [`Self::set_windows`].
    pub fn set_plugin_sections(&mut self, sections: Vec<PluginSection>) -> bool {
        if self.plugin == sections {
            return false;
        }
        self.plugin = sections;
        self.dirty = true;
        true
    }

    /// The counts [`row_model`] and [`hit_test`] derive the strip's shape
    /// from.
    #[must_use]
    pub fn counts(&self) -> SidebarCounts {
        SidebarCounts {
            needs_you: self.needs_you.len(),
            windows: self.windows.len(),
            roster: self.roster.len(),
            active_session: self.roster.iter().position(|s| s.active),
            host_starts: segment_starts(self.roster.iter().map(roster_segment)),
            project_starts: self.project_starts(),
            rule: self.rule,
            plugin: PluginShape::of(&self.plugin),
            agents_share: self.agents_share,
            agent_host_chip: self.agent_filter.host != HostFilter::Any
                || !agent_filter::agent_hosts(&self.agents).is_empty(),
        }
    }

    /// The click-resolution table for the current projections — what turns a
    /// [`SidebarHit`] index back into an action.
    #[must_use]
    pub fn click_targets(&self) -> SidebarTargets {
        SidebarTargets {
            counts: self.counts(),
            needs_you: self
                .needs_you
                .iter()
                .map(|e| {
                    e.session
                        .as_ref()
                        .map_or(SidebarTarget::Window(e.window), |name| {
                            SidebarTarget::Session {
                                name: name.clone(),
                                id: e.session_id,
                                window: e.pane.map(|_| e.window),
                                pane: e.pane,
                                resource: e.resource.clone(),
                            }
                        })
                })
                .collect(),
            roster: self
                .roster
                .iter()
                .map(|s| {
                    s.selectable.then(|| SessionRosterTarget {
                        name: s.name.clone(),
                        id: s.id,
                        host: s.route_host.clone(),
                        switch_host: s.switch_host.clone(),
                    })
                })
                .collect(),
            projects: self
                .roster
                .iter()
                .map(|s| s.group.as_ref().map(|group| group.key.clone()))
                .collect(),
            plugin: self
                .plugin
                .iter()
                .map(|section| {
                    section
                        .entries
                        .iter()
                        .map(|row| (row.window, row.pane))
                        .collect()
                })
                .collect(),
        }
    }

    /// Point (or clear) the live insertion marker at window `drop_at`.
    /// Same change-report contract as [`Self::set_windows`].
    pub fn set_drop_index(&mut self, drop_at: Option<usize>) -> bool {
        if self.drop_at == drop_at {
            return false;
        }
        self.drop_at = drop_at;
        self.dirty = true;
        true
    }

    /// Drop the paint cache so the next [`Self::paint`] re-emits even if its
    /// inputs are unchanged (e.g. after a full-frame clear).
    pub fn invalidate(&mut self) {
        self.last = None;
    }

    /// Paint the sidebar into `rect` (outer-viewport cells). No-op when the
    /// rect is empty or unchanged since the last paint.
    pub fn paint<W: Write>(&mut self, out: &mut W, rect: Rect) -> io::Result<()> {
        if rect.w == 0 || rect.h == 0 {
            return Ok(());
        }
        if !self.dirty && self.last.as_ref().is_some_and(|(r, _)| *r == rect) {
            return Ok(());
        }
        let buf = self.compose(rect, self.rule, &self.junctions);
        let previous = self
            .last
            .as_ref()
            .filter(|(r, _)| *r == rect)
            .map(|(_, b)| b);
        emit_changed(out, &buf, rect, previous)?;
        self.last = Some((rect, buf));
        self.dirty = false;
        Ok(())
    }

    /// Compose the strip into a `rect`-sized buffer at origin `(0, 0)`, the
    /// same compose the VT [`Self::paint`] path uses.
    #[must_use]
    pub fn compose_buffer(&self, rect: Rect, rule: SidebarRule, junctions: &[u16]) -> Buffer {
        self.compose(rect, rule, junctions)
    }

    /// A section header: label left, count on the shared right edge.
    fn header_line(&self, label: &str, count: usize, text_w: u16) -> Line<'static> {
        let right = if count == 0 {
            Vec::new()
        } else {
            vec![Span::styled(
                count.to_string(),
                Style::default().fg(self.theme.dim),
            )]
        };
        justify(
            vec![Span::styled(
                clip_text(label, usize::from(text_w)),
                Style::default()
                    .fg(self.theme.sidebar_section)
                    .add_modifier(Modifier::BOLD),
            )],
            right,
            text_w,
        )
    }

    /// Render a section's empty-state placeholder: the label
    /// nested one indent under the header, dim + italic so it reads as a
    /// quiet "nothing here yet" rather than a real, selectable row.
    fn empty_line(&self, label: &str, text_w: u16) -> Line<'static> {
        let label = clip_text(label, usize::from(text_w).saturating_sub(2));
        Line::from(Span::styled(
            format!("  {label}"),
            Style::default()
                .fg(self.theme.dim)
                .add_modifier(Modifier::ITALIC),
        ))
    }

    /// One window's row under its session: the agent badge (or a plain dot,
    /// filled when active, attention dot when asking), label, and branch
    /// right-aligned. The active row's bed is laid by [`Self::compose`].
    fn name_line(&self, w: &WindowInfo, text_w: u16) -> Line<'static> {
        let (glyph, glyph_style) = self.window_glyph(w);
        let exited = w.exited_marker();
        let exited_w = exited.as_ref().map_or(0, |marker| display_width(marker));
        // Reserve 2 cells for the ` !` attention
        // suffix so a long label can't push it off the strip.
        // Same for the retained-exit ` x` / ` xN` marker.
        let label_w = usize::from(text_w)
            .saturating_sub(4) // nested indent + glyph + space
            .saturating_sub(if w.attention { 2 } else { 0 })
            .saturating_sub(exited_w);
        let label = clip_text(&w.name, label_w);
        let style = if w.active {
            Style::default()
                .fg(self.theme.accent)
                .add_modifier(Modifier::BOLD)
        } else if w.exited.is_some() {
            Style::default().fg(self.theme.dim)
        } else {
            Style::default().fg(self.theme.text)
        };
        let mut left = vec![
            Span::raw("  "),
            Span::styled(format!("{glyph} "), glyph_style),
            Span::styled(label.clone(), style),
        ];
        // A window holding a pane that asked for a
        // human answer (ADR-0035) gets a themed `!` marker.
        if w.attention {
            left.push(Span::styled(
                " !",
                Style::default()
                    .fg(self.theme.attention)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if let Some(marker) = exited {
            left.push(Span::styled(marker, Style::default().fg(self.theme.dim)));
        }
        let used = 4 + display_width(&label) + if w.attention { 2 } else { 0 } + exited_w;
        let right = fitting_branch(w, usize::from(text_w).saturating_sub(used))
            .map(|branch| {
                vec![Span::styled(
                    branch.to_owned(),
                    Style::default().fg(self.theme.dim),
                )]
            })
            .unwrap_or_default();
        justify(left, right, text_w)
    }

    /// A window row's glyph and its style.
    fn window_glyph(&self, w: &WindowInfo) -> (String, Style) {
        if w.attention {
            return ("●".to_owned(), Style::default().fg(self.theme.attention));
        }
        if let Some(badge) = &w.badge {
            let mut style = Style::default().fg(badge
                .style
                .fg
                .as_deref()
                .and_then(|fg| fg.parse().ok())
                .unwrap_or(self.theme.dim));
            if badge.style.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            return (badge.glyph.clone(), style);
        }
        if w.active {
            ("●".to_owned(), Style::default().fg(self.theme.accent))
        } else {
            ("○".to_owned(), Style::default().fg(self.theme.dim))
        }
    }

    /// A machine segment header (ADR-0140): the host label in the chord
    /// tone, with `here` flush right on the machine this terminal is
    /// attached to. A machine that is down says so once, on its placeholder
    /// row. Host identity comes from the projection and is never inferred
    /// here.
    fn host_line(&self, s: &SessionRosterEntry, text_w: u16) -> Line<'static> {
        let here = s.switch_host.is_none() && !s.satellite;
        let tag = if here { "here" } else { "" };
        let label_budget = usize::from(text_w).saturating_sub(display_width(tag) + 1);
        let label = clip_text(&s.host, label_budget);
        let right = if tag.is_empty() {
            Vec::new()
        } else {
            vec![Span::styled(tag, Style::default().fg(self.theme.dim))]
        };
        justify(
            vec![Span::styled(
                label,
                Style::default()
                    .fg(self.theme.chord)
                    .add_modifier(Modifier::BOLD),
            )],
            right,
            text_w,
        )
    }

    /// One agent row: attention-ladder glyph and locator left, name right. An
    /// unseen `done` gets the bold diamond; a seen one relaxes to the idle
    /// ring; `working` gets the half ring.
    fn agent_line(&self, e: &AgentEntry, text_w: u16) -> Line<'static> {
        // ONE badge vocabulary for the whole chrome: the same call feeds
        // a pane's own title (`render::chrome::dividers`), so a working
        // agent can never be a `◐` here and something else there.
        let badge = crate::render::chrome::agent_badge(&self.theme, e.state, e.attention, e.seen);
        let host_label = e.host.as_deref().unwrap_or("");
        let host_cols = if host_label.is_empty() {
            0
        } else {
            display_width(host_label).saturating_add(1)
        };
        let avail = usize::from(text_w)
            .saturating_sub(ICON_COLUMNS)
            .saturating_sub(host_cols);
        // A satellite row's locator is already the agent name (there is no
        // attachable session to name). Painting that name again as the
        // suffix would repeat it. The glyph still carries the live state.
        let name = if e.host.is_some() && e.session.as_deref() == Some(e.name.as_str()) {
            ""
        } else {
            e.name.as_str()
        };
        // A cross-session row is labelled by its SESSION, not its window: the
        // row's job is to say where in the fleet to go, and a window name
        // out of its session's context ("edit") locates nothing.
        let locator = e.session.as_ref().unwrap_or(&e.window_name);
        // The destination earns at least half the row; the name gives way.
        let loc_budget = avail.saturating_sub(display_width(name) + 1).max(avail / 2);
        let locator = clip_text(locator, loc_budget);
        let name = clip_text(
            name,
            avail
                .saturating_sub(display_width(&locator))
                .saturating_sub(1),
        );
        let mut glyph_style = Style::default().fg(badge.color);
        if badge.emphatic {
            glyph_style = glyph_style.add_modifier(Modifier::BOLD);
        }
        let mut left = vec![Span::styled(format!("{} ", badge.glyph), glyph_style)];
        if let Some(host) = &e.host {
            left.push(Span::styled(
                format!("{host} "),
                Style::default().fg(self.theme.chord),
            ));
        }
        left.push(Span::styled(locator, Style::default().fg(self.theme.text)));
        let right = if name.is_empty() {
            Vec::new()
        } else {
            vec![Span::styled(name, Style::default().fg(self.theme.dim))]
        };
        justify(left, right, text_w)
    }

    /// One roster line: the session's dot, name, and a histogram of the
    /// panes that need a human (`●1 ◆2`) in the same glyphs and colours as
    /// agent badges. A satellite session paints a dim dot and no counts: its
    /// metadata is not subscribable here (`docs/spec/L3.md` §5).
    fn roster_line(&self, s: &SessionRosterEntry, text_w: u16) -> Line<'static> {
        let (dot, color) = self.roster_badge(s);
        let counts = self.roster_histogram(s);
        let counts_w: usize = counts.iter().map(|span| display_width(&span.content)).sum();
        let indent = roster_indent(s);
        let avail = usize::from(text_w).saturating_sub(2 + display_width(indent));
        // Identity keeps at least half the row even under a large histogram.
        let name_budget = avail
            .saturating_sub(counts_w + usize::from(counts_w > 0))
            .max(avail / 2);
        let name = clip_text(&s.name, name_budget);
        let style = if s.active {
            Style::default()
                .fg(self.theme.accent)
                .add_modifier(Modifier::BOLD)
        } else if s.selectable {
            Style::default().fg(self.theme.text)
        } else {
            Style::default().fg(self.theme.dim)
        };
        let mut left = vec![
            Span::styled(indent, Style::default().fg(self.theme.dim)),
            Span::styled(format!("{dot} "), Style::default().fg(color)),
            Span::styled(name, style),
        ];
        if let Some(host) = foreign_member_host(s) {
            let used: usize = left.iter().map(|span| display_width(&span.content)).sum();
            let room = usize::from(text_w).saturating_sub(used + counts_w + 1);
            if room > 2 {
                left.push(Span::styled(
                    clip_text(&format!(" @{host}"), room),
                    Style::default().fg(self.theme.chord),
                ));
            }
        }
        justify(left, counts, text_w)
    }

    /// The counts of the rungs that need a human, each in its own glyph and
    /// colour. Working and settled panes are quiet and not counted.
    fn roster_histogram(&self, s: &SessionRosterEntry) -> Vec<Span<'static>> {
        if s.satellite {
            return Vec::new();
        }
        let rungs = [
            (AGENT_BLOCKED, s.blocked, self.theme.agent_blocked),
            (AGENT_DONE, s.done_unvisited, self.theme.agent_done),
        ];
        let mut spans = Vec::new();
        for (glyph, n, color) in rungs.into_iter().filter(|(_, n, _)| *n > 0) {
            if !spans.is_empty() {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(
                format!("{glyph}{n}"),
                Style::default().fg(color),
            ));
        }
        spans
    }

    const fn roster_badge(&self, s: &SessionRosterEntry) -> (&'static str, ratatui::style::Color) {
        if s.satellite {
            ("○", self.theme.dim)
        } else {
            match s.top_rank() {
                4 => (AGENT_BLOCKED, self.theme.agent_blocked),
                3 => (AGENT_DONE, self.theme.agent_done),
                // Working is quiet: the dot only lights for what needs you.
                1 | 2 => ("○", self.theme.agent_idle),
                _ => ("○", self.theme.dim),
            }
        }
    }

    /// Render the `+N more` overflow row: dim and indented
    /// like an empty state, because it is chrome rather than a target you
    /// aim at — though clicking it does open the fleet dashboard.
    fn overflow_line(&self, hidden: usize, text_w: u16) -> Line<'static> {
        let label = clip_text(
            &format!("+{hidden} more"),
            usize::from(text_w).saturating_sub(2),
        );
        Line::from(Span::styled(
            format!("  {label}"),
            Style::default().fg(self.theme.dim),
        ))
    }

    /// A footer affordance row: the action glyph in the section register,
    /// the word in `dim`.
    fn affordance_line(&self, label: &str, text_w: u16) -> Line<'static> {
        let label = clip_text(label, usize::from(text_w));
        Line::from(self.affordance_spans(&label))
    }

    fn affordance_spans(&self, label: &str) -> Vec<Span<'static>> {
        let mut chars = label.chars();
        let glyph = chars.next().map(String::from).unwrap_or_default();
        let rest = chars.as_str().to_owned();
        vec![
            Span::styled(glyph, Style::default().fg(self.theme.chord)),
            Span::styled(rest, Style::default().fg(self.theme.dim)),
        ]
    }

    /// Render the sections + affordances + separator into a fresh
    /// `rect`-sized buffer, row-for-row from [`row_model`].
    fn compose(&self, rect: Rect, rule: SidebarRule, junctions: &[u16]) -> Buffer {
        let area = RataRect::new(0, 0, rect.w, rect.h);
        let mut buf = Buffer::empty(area);
        buf.set_style(
            area,
            Style::default().fg(self.theme.text).bg(self.theme.surface),
        );
        // One-cell gutters protect names from both the edge and separator.
        let text_w = text_width(rect.w);
        let counts = self.counts();
        let model = row_model(counts, rect.h);
        let hidden = hidden_counts(counts, &model);
        if text_w > 0 {
            let lines: Vec<Line<'static>> = model
                .iter()
                .map(|row| self.row_line(*row, hidden, text_w))
                .collect();
            let content_x = rule.content_x();
            Paragraph::new(lines).render(
                RataRect::new(content_x + GUTTER, 0, text_w, rect.h),
                &mut buf,
            );
        }
        self.paint_selection_bed(&mut buf, rect, rule, &model);
        paint_separator(&mut buf, rect, rule, &self.theme, junctions);
        self.paint_window_drop_marker(&mut buf, rect, rule);
        buf
    }

    /// Lay the selection bed under the active window's row, edge to edge
    /// of the strip (gutters included, the rule excluded), so the selected
    /// row is one band rather than a highlight hugging its glyphs.
    fn paint_selection_bed(
        &self,
        buf: &mut Buffer,
        rect: Rect,
        rule: SidebarRule,
        model: &[SidebarRow],
    ) {
        let Some(active) = self.windows.iter().position(|w| w.active) else {
            return;
        };
        let Some(y) = model
            .iter()
            .position(|row| *row == SidebarRow::WindowName(active))
            .and_then(|y| u16::try_from(y).ok())
        else {
            return;
        };
        let rule_x = rule.column(rect.w);
        for x in (0..rect.w).filter(|x| *x != rule_x) {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_bg(self.theme.selection_bg);
            }
        }
    }

    /// Reverse the window row under a live drag so the drop slot is
    /// visible before release. Leaves the separator rule alone so the
    /// handle stays a handle.
    fn paint_window_drop_marker(&self, buf: &mut Buffer, rect: Rect, rule: SidebarRule) {
        let Some(index) = self.drop_at else {
            return;
        };
        let model = row_model(self.counts(), rect.h);
        let Some(y) = model
            .iter()
            .position(|row| *row == SidebarRow::WindowName(index))
            .and_then(|y| u16::try_from(y).ok())
        else {
            return;
        };
        if y >= rect.h {
            return;
        }
        let rule_x = rule.column(rect.w);
        for x in 0..rect.w {
            if x == rule_x {
                continue;
            }
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_style(
                    Style::default()
                        .fg(self.theme.accent)
                        .add_modifier(Modifier::REVERSED),
                );
            }
        }
    }

    fn row_line(&self, row: SidebarRow, hidden: SidebarCounts, text_w: u16) -> Line<'static> {
        match row {
            SidebarRow::NeedsYouHeader => {
                self.header_line(NEEDS_YOU_HEADER, self.needs_you.len(), text_w)
            }
            SidebarRow::SpacesHeader => self.header_line(SPACES_HEADER, self.roster.len(), text_w),
            SidebarRow::PluginEmpty(_) => self.empty_line(PLUGIN_EMPTY, text_w),
            SidebarRow::NeedsYouEmpty => self.empty_line(
                if self.agent_filter.scope == AgentScope::Attention {
                    AGENTS_EMPTY_LABEL
                } else {
                    AGENTS_NONE_LABEL
                },
                text_w,
            ),
            SidebarRow::AgentFilter => self.agent_filter_line(text_w),
            SidebarRow::WindowName(i) => self
                .windows
                .get(i)
                .map_or_else(|| Line::from(""), |w| self.name_line(w, text_w)),
            SidebarRow::NeedsYou(j) => self
                .needs_you
                .get(j)
                .map_or_else(|| Line::from(""), |e| self.agent_line(e, text_w)),
            SidebarRow::RosterEntry(j) => self
                .roster
                .get(j)
                .map_or_else(|| Line::from(""), |s| self.roster_line(s, text_w)),
            SidebarRow::RosterHost(j) => self.roster.get(j).map_or_else(
                || Line::from(""),
                |s| {
                    s.group.as_ref().map_or_else(
                        || self.host_line(s, text_w),
                        |group| self.project_line(group, text_w),
                    )
                },
            ),
            SidebarRow::NeedsYouOverflow => self.overflow_line(hidden.needs_you, text_w),
            SidebarRow::RosterOverflow => self.sessions_overflow_line(hidden, text_w),
            SidebarRow::Blank => Line::from(""),
            SidebarRow::NewWindow => self.affordance_line(NEW_LABEL, text_w),
            SidebarRow::PluginHeader(s) => self.plugin.get(s).map_or_else(
                || Line::from(""),
                |section| self.header_line(&section.title, section.entries.len(), text_w),
            ),
            SidebarRow::PluginEntry(s, j) => self
                .plugin
                .get(s)
                .and_then(|section| section.entries.get(j))
                .map_or_else(
                    || Line::from(""),
                    |row| self.plugin_row_line(&row.text, text_w),
                ),
            SidebarRow::PluginOverflow(s) => {
                let hidden = self.plugin.get(s).map_or(0, |section| {
                    section
                        .entries
                        .len()
                        .saturating_sub(usize::from(section.rows).saturating_sub(1))
                });
                self.overflow_line(hidden, text_w)
            }
        }
    }

    /// The Agents filter row: each scope chip, the active one in the focus
    /// register on the selection bed, then the `@` machine chip when the
    /// agents span machines. Laid from [`agent_filter::filter_chips`], the
    /// same layout the hit test reads.
    fn agent_filter_line(&self, text_w: u16) -> Line<'static> {
        let chips = agent_filter::filter_chips(text_w, self.counts().agent_host_chip);
        let active = Style::default()
            .fg(self.theme.accent)
            .bg(self.theme.selection_bg)
            .add_modifier(Modifier::BOLD);
        let idle = Style::default().fg(self.theme.dim);
        let mut spans = Vec::with_capacity(chips.len() * 2);
        let mut x = 0u16;
        for (start, w, chip) in chips {
            if start > x {
                spans.push(Span::raw(" ".repeat(usize::from(start - x))));
            }
            let (label, on) = match chip {
                FilterChip::Scope(scope) => {
                    (scope.label().to_owned(), self.agent_filter.scope == scope)
                }
                FilterChip::Host => (
                    self.agent_filter.host.label(),
                    self.agent_filter.host != HostFilter::Any,
                ),
            };
            let label = clip_text(&label, usize::from(w));
            x = start + u16::try_from(display_width(&label)).unwrap_or(w);
            spans.push(Span::styled(label, if on { active } else { idle }));
        }
        Line::from(spans)
    }

    /// One plugin section row: a quiet bullet, then the rendered template.
    fn plugin_row_line(&self, text: &str, text_w: u16) -> Line<'static> {
        let label = clip_text(text, usize::from(text_w).saturating_sub(ICON_COLUMNS));
        Line::from(vec![
            Span::styled("· ", Style::default().fg(self.theme.dim)),
            Span::styled(label, Style::default().fg(self.theme.text)),
        ])
    }

    fn sessions_overflow_line(&self, hidden: SidebarCounts, text_w: u16) -> Line<'static> {
        let mut parts = Vec::new();
        if hidden.roster > 0 {
            parts.push(format!("+{} sessions", hidden.roster));
        }
        if hidden.windows > 0 {
            parts.push(format!("+{} tabs", hidden.windows));
        }
        self.affordance_line(&parts.join(", "), text_w)
    }
}

fn paint_separator(
    buf: &mut Buffer,
    rect: Rect,
    rule: SidebarRule,
    theme: &Theme,
    junctions: &[u16],
) {
    let sep_x = rule.column(rect.w);
    // The rail runs away from the strip, into the panes: rightward from a
    // trailing rule, leftward from a leading one.
    let tee = match rule {
        SidebarRule::Trailing => "├",
        SidebarRule::Leading => "┤",
    };
    for y in 0..rect.h {
        if let Some(cell) = buf.cell_mut((sep_x, y)) {
            cell.set_symbol(if junctions.contains(&y) { tee } else { "│" });
            cell.set_style(Style::default().fg(theme.border));
        }
    }
    // The collapse chevron claims the rule's bottom cell
    // whenever the footer renders (same condition as `hit_test`).
    if collapse_visible(rect)
        && let Some(cell) = buf.cell_mut((sep_x, rect.h - 1))
    {
        cell.set_symbol(COLLAPSE_GLYPH);
        cell.set_style(Style::default().fg(theme.dim));
    }
}

/// Count actual item rows, never host lines or padding, for honest overflow.
fn hidden_counts(counts: SidebarCounts, model: &[SidebarRow]) -> SidebarCounts {
    let mut hidden = counts;
    if counts.active_session.is_none_or(|i| i >= counts.roster) {
        hidden.windows = 0;
    }
    for row in model {
        match row {
            SidebarRow::NeedsYou(_) => hidden.needs_you = hidden.needs_you.saturating_sub(1),
            SidebarRow::RosterEntry(_) => hidden.roster = hidden.roster.saturating_sub(1),
            SidebarRow::WindowName(_) => hidden.windows = hidden.windows.saturating_sub(1),
            _ => {}
        }
    }
    hidden
}

/// Lay `left` flush left and `right` flush right on one row. Every row shares
/// this right edge; `right` is dropped whole rather than clipped when it
/// would touch `left`.
fn justify(left: Vec<Span<'static>>, right: Vec<Span<'static>>, text_w: u16) -> Line<'static> {
    let width = |spans: &[Span<'_>]| -> usize {
        spans.iter().map(|span| display_width(&span.content)).sum()
    };
    let (lw, rw) = (width(&left), width(&right));
    let w = usize::from(text_w);
    let mut spans = left;
    if rw > 0 && lw + 1 + rw <= w {
        spans.push(Span::raw(" ".repeat(w - lw - rw)));
        spans.extend(right);
    }
    Line::from(spans)
}

/// Emit `buf` at `rect`'s origin with per-cell SGR deltas, repainting whole
/// changed rows (clearing a shortened label's tail). Each row is one run
/// from its own CUP, so the cells spilled into by a wide glyph are skipped:
/// writing them advanced the cursor past the strip into the panes.
fn emit_changed<W: Write>(
    out: &mut W,
    buf: &Buffer,
    rect: Rect,
    previous: Option<&Buffer>,
) -> io::Result<()> {
    let mut changed = false;
    for row in 0..rect.h {
        if previous.is_some_and(|old| (0..rect.w).all(|col| old[(col, row)] == buf[(col, row)])) {
            continue;
        }
        changed = true;
        write!(out, "\x1b[{};{}H\x1b[0m", rect.y + row + 1, rect.x + 1)?;
        let mut prev_styled = None;
        let mut col = 0;
        while col < rect.w {
            let cell = &buf[(col, row)];
            crate::render::sgr::emit_cell_sgr(out, cell, &mut prev_styled)?;
            let sym = cell.symbol();
            let advance = if sym.is_empty() {
                out.write_all(b" ")?;
                1
            } else {
                out.write_all(sym.as_bytes())?;
                // Clamp: a symbol the strip believes is zero-width would
                // otherwise spin this loop forever.
                u16::try_from(crate::render::display_width(sym))
                    .unwrap_or(1)
                    .max(1)
            };
            col = col.saturating_add(advance);
        }
        out.write_all(b"\x1b[0m")?;
    }
    if changed { out.flush() } else { Ok(()) }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    const fn r(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect { x, y, w, h }
    }

    fn win(name: &str, active: bool) -> WindowInfo {
        WindowInfo {
            name: name.to_owned(),
            active,
            zoomed: false,
            attention: false,
            branch: None,
            exited: None,
            badge: None,
        }
    }

    fn win_attention(name: &str, active: bool) -> WindowInfo {
        WindowInfo {
            attention: true,
            ..win(name, active)
        }
    }

    fn win_exited(name: &str, active: bool, status: &str) -> WindowInfo {
        WindowInfo {
            exited: Some(status.to_owned()),
            ..win(name, active)
        }
    }

    fn win_branch(name: &str, active: bool, branch: &str) -> WindowInfo {
        WindowInfo {
            branch: Some(branch.to_owned()),
            ..win(name, active)
        }
    }

    fn agent(window: usize, window_name: &str, name: &str, state: AgentMetaState) -> AgentEntry {
        AgentEntry {
            session: None,
            session_id: None,
            resource: None,
            window,
            window_name: window_name.to_owned(),
            pane: None,
            name: name.to_owned(),
            state,
            attention: false,
            host: None,
            seen: false,
        }
    }

    fn roster(
        name: &str,
        blocked: usize,
        working: usize,
        done_unvisited: usize,
    ) -> SessionRosterEntry {
        SessionRosterEntry {
            name: name.to_owned(),
            host: "mini".to_owned(),
            selectable: true,
            blocked,
            working,
            done_unvisited,
            ..SessionRosterEntry::default()
        }
    }

    fn active_roster() -> SessionRosterEntry {
        SessionRosterEntry {
            active: true,
            ..roster("development", 0, 0, 0)
        }
    }

    #[test]
    fn roster_targets_preserve_host_routes_and_disable_placeholders() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_roster(vec![
            active_roster(),
            SessionRosterEntry {
                host: "devbox".to_owned(),
                route_host: Some("satellite-dev".to_owned()),
                satellite: true,
                ..roster("development", 0, 0, 0)
            },
            SessionRosterEntry {
                name: "unreachable".to_owned(),
                host: "offline-host".to_owned(),
                ..SessionRosterEntry::default()
            },
        ]);
        let targets = p.click_targets();
        assert_eq!(targets.counts.active_session, Some(0));
        assert_eq!(
            targets.roster,
            vec![
                Some(SessionRosterTarget {
                    name: "development".to_owned(),
                    id: None,
                    host: None,
                    switch_host: None,
                }),
                Some(SessionRosterTarget {
                    name: "development".to_owned(),
                    id: None,
                    host: Some("satellite-dev".to_owned()),
                    switch_host: None,
                }),
                None,
            ]
        );
        let rect = r(2, 3, 32, 18);
        let model = row_model(targets.counts, rect.h);
        let y = |row: SidebarRow| {
            rect.y + u16::try_from(model.iter().position(|r| *r == row).expect("row")).expect("y")
        };
        for (row, hit) in [
            (SidebarRow::RosterHost(0), SidebarHit::Roster(0)),
            (SidebarRow::RosterEntry(0), SidebarHit::Roster(0)),
            (SidebarRow::RosterHost(1), SidebarHit::Roster(1)),
            (SidebarRow::RosterEntry(1), SidebarHit::Roster(1)),
        ] {
            assert_eq!(hit_test(rect, targets.counts, 4, y(row)), Some(hit));
        }
    }

    #[test]
    fn changing_agent_state_repaints_only_that_stable_row() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_roster(vec![active_roster()]);
        let first = agent(0, "editor", "claude", AgentMetaState::Blocked);
        let second = agent(1, "runner", "codex", AgentMetaState::Done);
        p.set_agents(vec![first.clone(), second.clone()]);
        let rect = r(0, 0, 36, 18);
        paint_to_string(&mut p, rect);
        p.set_agents(vec![
            first,
            AgentEntry {
                state: AgentMetaState::Blocked,
                ..second
            },
        ]);
        let changed = paint_to_string(&mut p, rect);
        assert_eq!(rows_of(&changed).len(), 1);
        // Header, filter chips, then the agents: the second agent is row 4.
        assert!(changed.starts_with("\x1b[4;1H"));
        assert!(strip_ansi(&changed).contains("codex"));
        assert!(paint_to_string(&mut p, rect).is_empty());
    }

    #[test]
    fn overflow_counts_sessions_and_windows_without_counting_host_lines() {
        let c = SidebarCounts {
            active_session: Some(0),
            rule: SidebarRule::Trailing,
            ..counts(9, 5, 4)
        };
        let model = row_model(c, 14);
        let hidden = hidden_counts(c, &model);
        // The filter chip row takes one of the panel's rows.
        assert_eq!(hidden.needs_you, 5);
        assert_eq!(hidden.roster, 3);
        assert_eq!(hidden.windows, 3);
        assert!(model.contains(&SidebarRow::RosterOverflow));
        assert!(model.contains(&SidebarRow::RosterHost(0)));
        let p = SidebarPainter::new(Theme::default());
        let line = p.sessions_overflow_line(hidden, 32);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "+3 sessions, +3 tabs");
    }

    #[test]
    fn tiny_viewports_and_huge_counts_remain_bounded() {
        let c = SidebarCounts {
            needs_you: usize::MAX,
            windows: usize::MAX,
            roster: usize::MAX,
            active_session: Some(0),
            host_starts: u128::MAX,
            project_starts: 0,
            rule: SidebarRule::Trailing,
            plugin: PluginShape::default(),
            agents_share: DEFAULT_AGENTS_SHARE,
            agent_host_chip: false,
        };
        assert_eq!(row_model(c, 1), vec![SidebarRow::RosterOverflow]);
        let rect = r(0, 0, 20, 1);
        assert_eq!(hit_test(rect, c, 2, 0), Some(SidebarHit::Sessions));
        for h in 0..30 {
            let model = row_model(c, h);
            assert_eq!(model.len(), usize::from(h));
            assert_model_items(c, &model);
        }
        let invalid = SidebarCounts {
            active_session: Some(9),
            rule: SidebarRule::Trailing,
            ..counts(0, 100, 1)
        };
        assert!(
            !row_model(invalid, 18)
                .iter()
                .any(|r| matches!(r, SidebarRow::WindowName(_)))
        );
    }

    #[test]
    fn short_widths_clip_unicode_hosts_and_overflow_inside_separator() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win("编辑器", true)]);
        p.set_roster(vec![
            SessionRosterEntry {
                host: "日本語-devbox".to_owned(),
                ..active_roster()
            },
            roster("peer", 1, 2, 0),
        ]);
        p.set_agents(vec![
            agent(0, "编辑器", "claude", AgentMetaState::Blocked);
            12
        ]);
        for w in 0..20 {
            assert_narrow_paint(&mut p, w);
        }
    }

    fn assert_narrow_paint(p: &mut SidebarPainter, w: u16) {
        let rect = Rect {
            x: 0,
            y: 0,
            w,
            h: 14,
        };
        let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
        let painted = paint_to_string(p, rect);
        for row in rows_of(&painted) {
            assert_eq!(display_width(&row), usize::from(w), "w={w}: {row:?}");
        }
        if w > 0 {
            assert_eq!(buf[(w - 1, 0)].symbol(), "│");
        }
    }

    /// The attention ladder, rung by rung. The one that matters: an UNSEEN
    /// `done` agent outranks a `working` one — "finished but you haven't
    /// looked at it" is a request for a human; "still working" is not.
    #[test]
    fn attention_rank_puts_unreviewed_done_above_working() {
        use AgentMetaState as S;
        let blocked = attention_rank(S::Blocked, false, false);
        let done_unseen = attention_rank(S::Done, false, false);
        let working = attention_rank(S::Working, false, false);
        let done_seen = attention_rank(S::Done, false, true);
        let idle = attention_rank(S::Idle, false, true);
        let unknown = attention_rank(S::Unknown, false, true);

        assert!(blocked > done_unseen, "blocked outranks unreviewed done");
        assert!(done_unseen > working, "unreviewed done outranks working");
        assert!(working > done_seen, "working outranks a reviewed done");
        assert_eq!(done_seen, idle, "a reviewed done is as quiet as idle");
        assert!(idle > unknown, "an undeclared agent ranks last");

        // Visiting the pane is what demotes a finished agent — nothing else.
        assert!(attention_rank(S::Done, false, true) < attention_rank(S::Working, false, false));

        // An explicit attention flag (a declared high-attention record, or the
        // ADR-0035 asked flag) pins the row to the top rung whatever the state
        // says — an agent that asked for a human IS blocked on one.
        for state in [S::Idle, S::Working, S::Done, S::Unknown, S::Blocked] {
            assert_eq!(attention_rank(state, true, true), blocked, "{state:?}");
        }

        // `seen` is inert for every state but `done`: a blocked agent you
        // looked at is still blocked.
        for state in [S::Idle, S::Working, S::Blocked, S::Unknown] {
            assert_eq!(
                attention_rank(state, false, true),
                attention_rank(state, false, false),
                "{state:?}"
            );
        }
    }

    /// A roster row's dot and its agents' badges must agree about severity.
    /// They are two renderings of ONE ladder, so `top_rank` returns the same
    /// rungs `attention_rank` does — a session painted calm while one of its
    /// agents displays a blocked badge would discredit the whole strip.
    #[test]
    fn roster_top_rank_follows_the_attention_ladder() {
        use AgentMetaState as S;

        assert_eq!(
            roster("a", 1, 3, 2).top_rank(),
            attention_rank(S::Blocked, false, false),
            "one blocked pane pins the session to the blocked rung"
        );
        assert_eq!(
            roster("a", 0, 3, 2).top_rank(),
            attention_rank(S::Done, false, false),
            "unreviewed done outranks working at the session level too"
        );
        assert_eq!(
            roster("a", 0, 3, 0).top_rank(),
            attention_rank(S::Working, false, false),
        );

        let settled = SessionRosterEntry {
            settled: 4,
            ..roster("a", 0, 0, 0)
        };
        assert_eq!(settled.top_rank(), attention_rank(S::Idle, false, true));

        // A satellite session cannot be inspected from here (spec/L3.md §5),
        // so it lands on the bottom rung — explicitly unknown, never a calm
        // zero that reads as "nothing to see".
        let sat = SessionRosterEntry {
            unknown: 3,
            satellite: true,
            ..roster("prod-3", 0, 0, 0)
        };
        assert_eq!(sat.top_rank(), attention_rank(S::Unknown, false, true));
        assert_eq!(sat.total(), 3, "unknown panes still count toward the total");
        assert_eq!(roster("a", 1, 3, 2).total(), 6);
    }

    /// The unreviewed-`done` row must be visually distinct from both a
    /// `working` row and a reviewed-`done` row — the glyph is what the user
    /// scans for. Under the `need` filter only the unreviewed one stays.
    #[test]
    fn unreviewed_done_gets_its_own_glyph() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win("a", true)]);
        let rect = r(0, 0, 30, 12);

        // Agents tops the strip under its filter chips, so its only row is
        // index 2.
        let mut unseen = agent(0, "a", "claude", AgentMetaState::Done);
        unseen.seen = false;
        p.set_agents(vec![unseen.clone()]);
        let row = row_text(&p.compose_buffer(rect, SidebarRule::Trailing, &[]), rect, 2);
        assert!(row.contains('◆'), "unreviewed done: {row:?}");

        let seen = AgentEntry {
            seen: true,
            ..unseen
        };
        let text = |e: &AgentEntry| -> String {
            p.agent_line(e, 28)
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        };
        assert!(text(&seen).contains('○'), "reviewed done relaxes");
        let working = agent(0, "a", "claude", AgentMetaState::Working);
        assert!(text(&working).contains('◐'), "working");

        p.set_agents(vec![seen, working]);
        assert_eq!(p.counts().needs_you, 2, "`all` lists both");
        p.set_agent_filter(AgentFilter {
            scope: AgentScope::Attention,
            host: HostFilter::Any,
        });
        assert_eq!(p.counts().needs_you, 0, "neither needs a human");
    }

    /// An agent row carries the shared glyph for its state: an unread
    /// `done` is the diamond and `working` the half ring.
    #[test]
    fn agent_rows_use_the_shared_state_glyphs() {
        let p = SidebarPainter::new(Theme::default());
        let text = |e: &AgentEntry| -> String {
            p.agent_line(e, 28)
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        };
        let done = agent(0, "a", "claude", AgentMetaState::Done);
        assert!(text(&done).contains('◆'), "done");
        let working = agent(0, "a", "claude", AgentMetaState::Working);
        assert!(text(&working).contains('◐'), "working");
    }

    /// `seen` is a real display input, so a flip must bust the paint cache —
    /// otherwise visiting a finished pane would leave the "look at me" glyph
    /// on screen.
    #[test]
    fn seen_flip_busts_the_paint_cache() {
        let mut p = SidebarPainter::new(Theme::default());
        let done = agent(0, "a", "claude", AgentMetaState::Done);
        assert!(p.set_agents(vec![done.clone()]));
        assert!(!p.set_agents(vec![done.clone()]));
        assert!(p.set_agents(vec![AgentEntry { seen: true, ..done }]));
    }

    /// phux-l96p.8 fix pass: a row must advance the terminal's cursor
    /// exactly `rect.w` columns.
    ///
    /// The strip emits each row as one uninterrupted run from its own
    /// `CUP`, so its column budget is only as good as its idea of how
    /// wide each glyph is. A DOUBLE-WIDTH character advances two columns
    /// and ratatui leaves the cell it spills into empty; writing a space
    /// there advanced the row one column too far per wide character, and
    /// a CJK window name walked the whole strip out of its reserved
    /// columns and over the panes beside it — libghostty's cells
    /// (ADR-0020).
    #[test]
    fn a_wide_window_name_stays_inside_the_strip() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_roster(vec![active_roster()]);
        assert!(p.set_windows(vec![win("日本語のペイン名前がとても長い", true)]));
        let rect = r(0, 0, 20, 12);
        let painted = paint_to_string(&mut p, rect);
        // Each emitted row is one run from one CUP, so the emitted
        // GLYPH widths per row must total the strip's width exactly —
        // one column too many and the next row starts inside a pane.
        let rows = rows_of(&painted);
        assert_eq!(rows.len(), usize::from(rect.h), "one run per strip row");
        for row in rows {
            assert_eq!(
                crate::render::display_width(&row),
                usize::from(rect.w),
                "row {row:?} does not advance the cursor exactly {} columns",
                rect.w
            );
        }
    }

    /// Split an emitted strip into its per-row glyph runs (the text
    /// between one row's `CUP` and the next).
    fn rows_of(painted: &str) -> Vec<String> {
        let mut rows: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut chars = painted.chars().peekable();
        let mut started = false;
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                }
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        if d == 'H' {
                            if started {
                                rows.push(std::mem::take(&mut cur));
                            }
                            started = true;
                        }
                        break;
                    }
                }
                continue;
            }
            if started {
                cur.push(c);
            }
        }
        if started {
            rows.push(cur);
        }
        rows
    }

    fn paint_to_string(painter: &mut SidebarPainter, rect: Rect) -> String {
        let mut out = Vec::new();
        painter.paint(&mut out, rect).expect("paint");
        String::from_utf8(out).expect("utf8")
    }

    /// Strip CSI escape sequences so an assertion can read the plain glyphs —
    /// a styled (active) row interleaves a per-cell SGR between every cell, so
    /// its label is not a contiguous substring of the raw byte stream.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                // CSI is `ESC [ params... final`; the final byte of the
                // sequences we emit (`H`, `m`) is an ASCII letter, while the
                // introducer `[`, digits, and `;` are not — consume through
                // the first letter.
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// Row `y` of the composed buffer as plain text (separator column
    /// excluded).
    fn row_text(buf: &Buffer, rect: Rect, y: u16) -> String {
        (0..rect.w.saturating_sub(1))
            .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
            .collect()
    }

    #[test]
    fn renders_each_window_label() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_roster(vec![active_roster()]);
        p.set_windows(vec![win("editor", false), win("shell", true)]);
        let raw = paint_to_string(&mut p, r(0, 0, 20, 14));
        let plain = strip_ansi(&raw);
        assert!(plain.contains("editor"), "first tab label: {plain:?}");
        assert!(plain.contains("shell"), "second tab label: {plain:?}");
        assert!(plain.contains(SPACES_HEADER), "sessions header: {plain:?}");
        // The active window gets the filled status dot.
        assert!(plain.contains('●'), "active dot missing: {plain:?}");
        assert!(plain.contains('○'), "inactive dot missing: {plain:?}");
        // Separator rule present.
        assert!(plain.contains('│'), "separator missing: {plain:?}");
    }

    #[test]
    fn unchanged_repaint_is_a_no_op() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_roster(vec![active_roster()]);
        p.set_windows(vec![win("a", true)]);
        let rect = r(0, 0, 16, 12);
        assert!(
            !paint_to_string(&mut p, rect).is_empty(),
            "first paint emits"
        );
        // Same inputs ⇒ cached ⇒ nothing emitted.
        assert!(
            paint_to_string(&mut p, rect).is_empty(),
            "unchanged repaint must emit nothing"
        );
        // A window change invalidates the cache.
        p.set_windows(vec![win("b", true)]);
        assert!(
            !paint_to_string(&mut p, rect).is_empty(),
            "changed windows must re-emit"
        );
        // Branches that fit repaint; omitted long branches stay invisible.
        p.set_agents(vec![agent(0, "b", "claude", AgentMetaState::Idle)]);
        paint_to_string(&mut p, rect);
        p.set_windows(vec![win_branch("b", true, "main")]);
        assert!(!paint_to_string(&mut p, rect).is_empty());
        p.set_windows(vec![win_branch("b", true, "a-branch-too-long-to-fit")]);
        paint_to_string(&mut p, rect);
        p.set_windows(vec![win_branch(
            "b",
            true,
            "another-branch-too-long-to-fit",
        )]);
        assert!(
            paint_to_string(&mut p, rect).is_empty(),
            "hidden branch context must not re-emit identical cells"
        );
        assert!(!paint_to_string(&mut p, Rect { h: 14, ..rect }).is_empty());
    }

    #[test]
    fn host_update_emits_only_its_row_and_invalidation_restores_every_row() {
        let mut p = SidebarPainter::new(Theme::default());
        // Band off: this test is about the roster rows.
        p.set_agents_share(0);
        let rect = r(7, 2, 36, 24);
        let peer = SessionRosterEntry {
            host: "xps".to_owned(),
            ..roster("peer", 0, 0, 0)
        };
        p.set_windows(vec![win("editor", true)]);
        p.set_roster(vec![active_roster(), peer.clone()]);
        let full = paint_to_string(&mut p, rect);
        p.set_roster(vec![
            SessionRosterEntry {
                host: "devbox".to_owned(),
                ..active_roster()
            },
            peer,
        ]);
        let changed = paint_to_string(&mut p, rect);
        assert_eq!(
            changed.matches('H').count(),
            1,
            "one CUP for the changed host row"
        );
        // Sessions tops a strip with no agents; the host line is row 1.
        assert!(changed.starts_with("\x1b[4;8H"));
        assert!(strip_ansi(&changed).contains("devbox"));
        assert!(!changed.contains("editor"));
        assert!(
            changed.len() * 8 < full.len(),
            "one row costs much less than the whole strip"
        );
        p.invalidate();
        assert_eq!(paint_to_string(&mut p, rect).matches('H').count(), 24);
        assert!(paint_to_string(&mut p, rect).is_empty());
        assert_eq!(
            paint_to_string(&mut p, Rect { x: 8, ..rect })
                .matches('H')
                .count(),
            24
        );
    }

    #[test]
    fn unicode_roster_counts_align_and_selection_owns_its_gutters() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_roster(vec![active_roster()]);
        p.set_windows(vec![win_branch("编辑器", true, "cafe\u{301}")]);
        let rect = r(0, 0, 36, 12);
        // The active tab's row, under the fixed Agents panel.
        let active_row = u16::try_from(
            row_model(p.counts(), rect.h)
                .iter()
                .position(|row| *row == SidebarRow::WindowName(0))
                .expect("the active window row"),
        )
        .expect("row fits");
        let b = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
        for y in [0, 1] {
            assert_eq!(b[(0, y)].bg, p.theme.surface);
            assert_eq!(b[(34, y)].bg, p.theme.surface);
            assert_eq!(b[(35, y)].bg, p.theme.surface);
        }
        assert_eq!(b[(0, active_row)].bg, p.theme.selection_bg);
        assert_eq!(b[(34, active_row)].bg, p.theme.selection_bg);
        assert_eq!(b[(35, active_row)].bg, p.theme.surface);
        for name in ["构建工具", "cafe\u{301}", "build"] {
            let line = p.roster_line(&roster(name, 1, 2, 0), 30);
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(display_width(&text), 30);
            assert!(text.ends_with("●1"), "working is not counted: {text:?}");
        }
    }

    #[test]
    fn compact_attention_rows_keep_same_session_agents_distinguishable() {
        let p = SidebarPainter::new(Theme::default());
        let text = |name| {
            let mut e = agent(0, "work", name, AgentMetaState::Blocked);
            e.session = Some("development".to_owned());
            p.agent_line(&e, 25)
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        assert!(text("claude").contains("claude"));
        assert!(text("codex").contains("codex"));
        assert_ne!(text("claude"), text("codex"));
    }

    /// An asking window (ADR-0035) carries `!`; a retained exited pane
    /// (ADR-0124) carries `x<status>`. Either flip re-emits.
    #[test]
    fn window_markers_for_attention_and_retained_exit() {
        for (marked, text) in [
            (win_attention("shell", false), "shell !"),
            (win_exited("shell", false, "3"), "shell x3"),
        ] {
            let mut p = SidebarPainter::new(Theme::default());
            p.set_roster(vec![active_roster()]);
            p.set_windows(vec![win("editor", true), win("shell", false)]);
            let rect = r(0, 0, 20, 14);
            let plain = strip_ansi(&paint_to_string(&mut p, rect));
            assert!(
                !plain.contains("shell !") && !plain.contains(" x"),
                "{plain:?}"
            );
            assert!(p.set_windows(vec![win("editor", true), marked]));
            let plain = strip_ansi(&paint_to_string(&mut p, rect));
            assert!(plain.contains(text), "{plain:?}");
        }
    }

    /// An identical window list reports no change (the agent-event chrome
    /// path gates its repaint on this).
    #[test]
    fn set_windows_reports_change_only_on_difference() {
        let mut p = SidebarPainter::new(Theme::default());
        assert!(p.set_windows(vec![win("a", true)]));
        assert!(!p.set_windows(vec![win("a", true)]));
        assert!(p.set_windows(vec![win_attention("a", true)]));
        // Branch changes still update projection equality even though compact
        // window rows omit branch context and emit no changed cells.
        assert!(p.set_windows(vec![win_branch("a", true, "main")]));
        assert!(p.set_windows(vec![win_branch("a", true, "feature")]));
        // Same contract for the agents section — a state
        // flip (idle -> working) must repaint.
        let done = agent(0, "a", "claude", AgentMetaState::Done);
        assert!(p.set_agents(vec![done.clone()]));
        assert!(!p.set_agents(vec![done]));
        assert!(p.set_agents(vec![agent(0, "a", "claude", AgentMetaState::Blocked)]));
        // Under `all` every lifecycle reaches the list, so each flip reports.
        assert!(p.set_agents(vec![agent(0, "a", "claude", AgentMetaState::Working)]));
        assert!(p.set_agents(vec![agent(0, "a", "claude", AgentMetaState::Idle)]));
        // A filter that moved reports too; the same filter again does not.
        let need = AgentFilter {
            scope: AgentScope::Attention,
            host: HostFilter::Any,
        };
        assert!(p.set_agent_filter(need.clone()));
        assert!(!p.set_agent_filter(need));
    }

    /// Tabs expand compactly beneath the current session; one machine
    /// paints no host line.
    #[test]
    fn windows_nest_under_active_session_without_extra_sections() {
        let mut p = SidebarPainter::new(Theme::default());
        // Band off: this test is about window nesting under the session.
        p.set_agents_share(0);
        p.set_roster(vec![active_roster(), roster("peer", 0, 0, 0)]);
        p.set_windows(vec![
            win_branch("phux", true, "wave2/herdr"),
            win("scratch", false),
        ]);
        let rect = r(0, 0, 20, 18);
        let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
        assert!(row_text(&buf, rect, 0).contains(SPACES_HEADER));
        assert!(row_text(&buf, rect, 1).contains("development"));
        assert!(row_text(&buf, rect, 2).starts_with("   ● phux"));
        assert!(row_text(&buf, rect, 3).starts_with("   ○ scratch"));
        assert!(row_text(&buf, rect, 4).contains("peer"));
        let text = strip_text(&p, rect);
        assert!(!text.contains("mini"), "{text}");
        assert!(!text.contains("wave2/herdr"));
    }

    /// A live window-row drag paints the insertion marker on
    /// the pointed-at row and clears it when the index is dropped.
    #[test]
    fn window_drop_marker_follows_the_index_and_clears() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_roster(vec![active_roster()]);
        p.set_windows(vec![win("phux", true), win("scratch", false)]);
        let rect = r(0, 0, 20, 18);
        let model = row_model(p.counts(), rect.h);
        let y0 = window_row_y(&model, 0);
        let y1 = window_row_y(&model, 1);
        let unmarked = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
        assert!(
            !row_reversed(&unmarked, rect, y0) && !row_reversed(&unmarked, rect, y1),
            "no drag, no marker"
        );
        assert!(p.set_drop_index(Some(1)));
        assert!(!p.set_drop_index(Some(1)));
        let marked = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
        assert!(
            row_reversed(&marked, rect, y1),
            "drop index 1 must reverse that window row"
        );
        assert!(
            !row_reversed(&marked, rect, y0),
            "the other window row stays unmarked"
        );
        assert!(p.set_drop_index(None));
        let cleared = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
        assert!(
            !row_reversed(&cleared, rect, y1),
            "clearing the index must drop the marker"
        );
    }

    fn window_row_y(model: &[SidebarRow], index: usize) -> u16 {
        model
            .iter()
            .position(|row| *row == SidebarRow::WindowName(index))
            .and_then(|y| u16::try_from(y).ok())
            .expect("window row is on the strip")
    }

    fn row_reversed(buf: &Buffer, rect: Rect, y: u16) -> bool {
        (0..rect.w.saturating_sub(1)).any(|x| buf[(x, y)].modifier.contains(Modifier::REVERSED))
    }

    /// Every lifecycle stays in the caller's supplied display order.
    #[test]
    fn agents_section_renders_state_and_name_rows() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win("phux", true), win("scratch", false)]);
        p.set_roster(vec![active_roster()]);
        p.set_agents(vec![
            agent(0, "phux", "claude", AgentMetaState::Blocked),
            agent(1, "scratch", "merge-queue-w5", AgentMetaState::Done),
        ]);
        let rect = r(0, 0, 36, 14);
        let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
        assert!(
            row_text(&buf, rect, 0).contains(NEEDS_YOU_HEADER),
            "the queue tops the strip: {:?}",
            row_text(&buf, rect, 0)
        );
        assert!(
            row_text(&buf, rect, 1).contains(AgentScope::All.label()),
            "the filter chips sit under the header"
        );
        let claude_row = row_text(&buf, rect, 2);
        assert!(
            claude_row.contains("phux") && claude_row.contains("claude"),
            "queue row shows locator + state - name: {claude_row:?}"
        );
        let worker_row = row_text(&buf, rect, 3);
        assert!(
            worker_row.contains("scratch") && worker_row.contains("merge-queue-w5"),
            "second queue row: {worker_row:?}"
        );
        assert!(
            row_text(&buf, rect, 7).contains(SPACES_HEADER),
            "Sessions lands on the fixed line below the Agents panel: {:?}",
            row_text(&buf, rect, 7)
        );
        p.set_agents(vec![
            agent(0, "phux", "claude", AgentMetaState::Done),
            agent(1, "scratch", "merge-queue-w5", AgentMetaState::Blocked),
        ]);
        let changed = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
        assert!(row_text(&changed, rect, 2).contains("claude"));
        assert!(row_text(&changed, rect, 3).contains("merge-queue-w5"));
        assert_eq!(row_text(&buf, rect, 7), row_text(&changed, rect, 7));
    }

    /// A cross-session queue row is labelled by its SESSION, not
    /// by its window — "edit" locates nothing once the row can come from
    /// anywhere on the server.
    #[test]
    fn a_foreign_queue_row_is_labelled_by_its_session() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win("phux", true)]);
        p.set_agents(vec![AgentEntry {
            session: Some("phux-feat-auth".to_owned()),
            pane: Some(1),
            ..agent(0, "edit", "claude", AgentMetaState::Blocked)
        }]);
        let rect = r(0, 0, 36, 14);
        let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
        let row = row_text(&buf, rect, 2);
        assert!(
            row.contains("phux-feat-auth"),
            "foreign row names its session: {row:?}"
        );
        assert!(
            !row.contains("edit"),
            "window name is not the locator: {row:?}"
        );
    }

    /// ADR-0136: a satellite agent row badges its host and opens that pane.
    #[test]
    fn a_satellite_agent_row_badges_its_host_and_opens_the_pane() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win("phux", true)]);
        let resource = ResourceId::satellite("gpubox", 4);
        p.set_agents(vec![AgentEntry {
            session: Some("reviewer".to_owned()),
            resource: Some(resource.clone()),
            host: Some("gpubox".to_owned()),
            ..agent(0, "edit", "reviewer", AgentMetaState::Blocked)
        }]);
        let rect = r(0, 0, 36, 14);
        let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
        // Agents on a satellite add the `@` machine chip to the filter row.
        assert!(row_text(&buf, rect, 1).contains("@all"));
        let row = row_text(&buf, rect, 2);
        assert!(row.contains("gpubox"), "host badge: {row:?}");
        assert!(row.contains("reviewer"), "agent name: {row:?}");
        let action = satellite_open_action(&resource).expect("satellite click opens");
        assert_eq!(action.action, "split-pane");
        assert_eq!(
            action.args.get("resource").and_then(|value| value.as_str()),
            Some("gpubox/@4")
        );
        assert!(satellite_open_action(&ResourceId::local(1)).is_none());
    }

    /// A queue click must carry the peer's `SessionId` so a
    /// rename of that session cannot retarget the switch.
    #[test]
    fn a_foreign_queue_click_carries_session_identity() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_agents(vec![AgentEntry {
            session: Some("stale".to_owned()),
            session_id: Some(SessionId::new(7)),
            pane: Some(1),
            ..agent(0, "edit", "claude", AgentMetaState::Blocked)
        }]);
        assert_eq!(
            p.click_targets().needs_you,
            vec![SidebarTarget::Session {
                name: "stale".to_owned(),
                id: Some(SessionId::new(7)),
                window: Some(0),
                pane: Some(1),
                resource: None,
            }]
        );
        // A graph-discovered agent carries its `ResourceId` and no invented
        // window/pane indices.
        p.set_agents(vec![AgentEntry {
            session: Some("peer".to_owned()),
            session_id: Some(SessionId::new(2)),
            resource: Some(ResourceId::local(10)),
            pane: None,
            ..agent(0, "main", "reviewer", AgentMetaState::Blocked)
        }]);
        assert_eq!(
            p.click_targets().needs_you,
            vec![SidebarTarget::Session {
                name: "peer".to_owned(),
                id: Some(SessionId::new(2)),
                window: None,
                pane: None,
                resource: Some(ResourceId::local(10)),
            }]
        );
    }

    /// ADR-0112: a calm fleet keeps the Agents panel (header and quiet
    /// placeholder) so Sessions never moves.
    #[test]
    fn a_quiet_fleet_keeps_the_agents_panel() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win_branch("phux", true, "main")]);
        p.set_roster(vec![active_roster()]);
        p.set_agents(vec![agent(0, "phux", "claude", AgentMetaState::Working)]);
        p.set_agent_filter(AgentFilter {
            scope: AgentScope::Attention,
            host: HostFilter::Any,
        });
        let rect = r(0, 0, 24, 12);
        let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
        assert!(
            row_text(&buf, rect, 0).contains(NEEDS_YOU_HEADER),
            "the Agents panel keeps its header when calm: {:?}",
            row_text(&buf, rect, 0)
        );
        assert!(
            row_text(&buf, rect, 2).contains(AGENTS_EMPTY_LABEL),
            "a quiet panel shows its placeholder: {:?}",
            row_text(&buf, rect, 2)
        );
        // Under any other filter an empty panel says nothing is running.
        p.set_agents(Vec::new());
        p.set_agent_filter(AgentFilter::default());
        let empty = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
        assert!(row_text(&empty, rect, 2).contains(AGENTS_NONE_LABEL));
        let sessions = row_model(p.counts(), rect.h)
            .iter()
            .position(|r| *r == SidebarRow::SpacesHeader)
            .expect("sessions laid");
        assert_eq!(
            sessions,
            agents_budget(DEFAULT_AGENTS_SHARE, usize::from(rect.h) - 1),
            "Sessions sits on the fixed line whatever the population"
        );
        assert_eq!(p.counts().needs_you, 0);
    }

    /// `agents-share` is a live input: `0` drops the band, a larger share
    /// pushes Sessions down, and changing it dirties the strip so a caller
    /// gating on the report repaints.
    #[test]
    fn agents_share_knob_controls_the_band() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win("phux", true)]);
        p.set_roster(vec![active_roster()]);
        p.set_agents(vec![agent(0, "phux", "claude", AgentMetaState::Blocked)]);
        let rect = r(0, 0, 24, 12);
        let sessions_at = |p: &SidebarPainter| {
            row_model(p.counts(), rect.h)
                .iter()
                .position(|r| *r == SidebarRow::SpacesHeader)
                .expect("sessions laid")
        };
        assert_eq!(row_model(p.counts(), rect.h)[0], SidebarRow::NeedsYouHeader);
        let banded = sessions_at(&p);
        assert!(p.set_agents_share(0), "a change reports");
        assert!(!p.set_agents_share(0), "the same share is a no-op");
        assert_eq!(
            row_model(p.counts(), rect.h)[0],
            SidebarRow::SpacesHeader,
            "0 drops the Agents band"
        );
        assert!(p.set_agents_share(80));
        assert!(
            sessions_at(&p) > banded,
            "a larger share pushes Sessions down"
        );
    }

    /// With nothing to list, the strip paints no section headers at all.
    #[test]
    fn an_empty_strip_paints_no_headers() {
        let p = SidebarPainter::new(Theme::default());
        let rect = r(0, 0, 24, 12);
        let text = strip_text(&p, rect);
        assert!(!text.contains(SPACES_HEADER), "{text}");
        assert!(!text.contains(NEEDS_YOU_HEADER), "{text}");
        assert_eq!(hit_test(rect, p.counts(), 3, 0), None);
    }

    /// Each session has a histogram name row and explicit host identity.
    #[test]
    fn the_roster_renders_host_pairs_with_counts() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win("phux", true)]);
        p.set_roster(vec![
            roster("feat-auth", 1, 2, 0),
            SessionRosterEntry {
                unknown: 4,
                satellite: true,
                host: "devbox".to_owned(),
                ..roster("prod-3", 0, 0, 0)
            },
        ]);
        let rect = r(0, 0, 26, 16);
        let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
        let model = row_model(p.counts(), rect.h);
        let first = model
            .iter()
            .position(|r| matches!(r, SidebarRow::RosterEntry(0)))
            .expect("roster row 0 allocated");
        let busy = row_text(&buf, rect, u16::try_from(first).unwrap());
        assert!(busy.contains("feat-auth"), "session name: {busy:?}");
        assert!(
            busy.contains("●1") && !busy.contains('◐'),
            "histogram counts only what needs a human: {busy:?}"
        );
        // Each machine's header sits directly above its sessions.
        assert!(row_text(&buf, rect, u16::try_from(first - 1).unwrap()).contains("mini"));
        assert!(row_text(&buf, rect, u16::try_from(first + 1).unwrap()).contains("devbox"));
        let sat = row_text(&buf, rect, u16::try_from(first + 2).unwrap());
        assert!(sat.contains("prod-3"), "satellite name: {sat:?}");
        assert!(!sat.contains('?'), "no unknown-count noise: {sat:?}");
    }

    fn strip_text(p: &SidebarPainter, rect: Rect) -> String {
        let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
        let mut out = String::new();
        for y in 0..rect.h {
            let mut row: String = (0..rect.w)
                .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect();
            row.truncate(row.trim_end().len());
            out.push_str(&row);
            out.push('\n');
        }
        out
    }

    /// Calm population retains both fixed areas and expands the active session.
    #[test]
    fn sectioned_layout_snapshot_quiet() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win_branch("phux", true, "main")]);
        p.set_roster(vec![
            active_roster(),
            roster("feat-auth", 0, 1, 0),
            SessionRosterEntry {
                unknown: 2,
                satellite: true,
                host: "devbox".to_owned(),
                ..roster("prod-3", 0, 0, 0)
            },
        ]);
        let rect = r(0, 0, 26, 18);
        insta::assert_snapshot!(strip_text(&p, rect));
    }

    /// A full Agents area overflows without moving Sessions.
    #[test]
    fn sectioned_layout_snapshot_attention() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win_branch("phux", true, "main")]);
        let mut queue = vec![
            agent(0, "phux", "codex", AgentMetaState::Blocked),
            AgentEntry {
                session: Some("feat-auth".to_owned()),
                pane: Some(0),
                ..agent(1, "edit", "claude", AgentMetaState::Blocked)
            },
        ];
        for i in 0..7 {
            queue.push(AgentEntry {
                session: Some(format!("wave-{i}")),
                pane: Some(0),
                ..agent(0, "run", "claude", AgentMetaState::Working)
            });
        }
        p.set_agents(queue);
        p.set_roster(vec![active_roster(), roster("feat-auth", 1, 0, 0)]);
        let rect = r(0, 0, 26, 18);
        insta::assert_snapshot!(strip_text(&p, rect));
    }

    /// The New window footer renders on the strip's last row
    /// when the strip is tall enough, and drops out below the minimum.
    /// The collapse chevron claims the bottom corner cell.
    #[test]
    fn footer_affordance_renders_on_the_last_row() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win("shell", true)]);
        let rect = r(0, 0, 28, 8);
        let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
        assert!(
            row_text(&buf, rect, 7).contains(NEW_LABEL),
            "row 7 should hold the new affordance: {:?}",
            row_text(&buf, rect, 7)
        );
        // The bottom corner cell carries the collapse chevron instead of
        // the separator rule.
        assert_eq!(buf[(27, 7)].symbol(), COLLAPSE_GLYPH);
        assert_eq!(buf[(27, 6)].symbol(), "│");
        // A 3-row strip is below the footer minimum: no affordances, no
        // chevron.
        let short = r(0, 0, 20, 3);
        let plain = strip_ansi(&paint_to_string(&mut p, short));
        assert!(
            !plain.contains(NEW_LABEL),
            "short strip must not render the footer: {plain:?}"
        );
        assert!(
            !plain.contains(COLLAPSE_GLYPH),
            "short strip must not render the chevron: {plain:?}"
        );
    }

    /// A right-docked strip puts its rule (and the collapse
    /// chevron) on its pane-facing first column, and every row target moves
    /// one column away from it, out to the screen edge.
    #[test]
    fn a_leading_rule_mirrors_the_strip_toward_the_panes() {
        let rect = r(52, 0, 28, 14);
        let leading = SidebarCounts {
            active_session: Some(0),
            rule: SidebarRule::Leading,
            agents_share: 0,
            ..counts(0, 1, 1)
        };
        assert_eq!(
            hit_test(rect, leading, 52, 3),
            None,
            "the rule is no row target"
        );
        assert_eq!(hit_test(rect, leading, 52, 13), Some(SidebarHit::Collapse));
        assert_eq!(
            hit_test(rect, leading, 79, 3),
            Some(SidebarHit::Window(0)),
            "the screen-edge column is row content, not a rule"
        );
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win("build", true)]);
        let local = Rect { x: 0, ..rect };
        let buf = p.compose_buffer(local, SidebarRule::Leading, &[]);
        assert_eq!(
            buf.cell((0, 0)).map(ratatui::buffer::Cell::symbol),
            Some("│")
        );
        assert_ne!(
            buf.cell((27, 0)).map(ratatui::buffer::Cell::symbol),
            Some("│")
        );
        assert_eq!(
            buf.cell((0, 13)).map(ratatui::buffer::Cell::symbol),
            Some(COLLAPSE_GLYPH)
        );
    }

    // ---------- phux-fce4 / phux-foz.9: row model + hit-test ----------

    fn counts(needs_you: usize, windows: usize, roster: usize) -> SidebarCounts {
        SidebarCounts {
            needs_you,
            windows,
            roster,
            active_session: None,
            host_starts: every_segment(roster),
            project_starts: 0,
            rule: SidebarRule::Trailing,
            plugin: PluginShape::default(),
            agents_share: DEFAULT_AGENTS_SHARE,
            agent_host_chip: false,
        }
    }

    /// Every entry on its own machine: one header per session, the densest
    /// shape the strip can take.
    fn every_segment(roster: usize) -> u128 {
        (0..roster.min(128)).fold(0, |mask, j| mask | (1u128 << j))
    }

    #[test]
    fn row_model_reserves_footer_and_truncates_blocks() {
        // Band off: this test is about the footer and the session blocks.
        let c = SidebarCounts {
            active_session: Some(0),
            rule: SidebarRule::Trailing,
            agents_share: 0,
            ..counts(0, 3, 1)
        };
        let rows = row_model(c, 9);
        assert_eq!(rows.len(), 9);
        assert_eq!(rows[0], SidebarRow::SpacesHeader);
        assert_eq!(rows[1], SidebarRow::RosterHost(0));
        assert_eq!(rows[2], SidebarRow::RosterEntry(0));
        assert_eq!(rows[5], SidebarRow::WindowName(2));
        assert_eq!(rows[8], SidebarRow::NewWindow);
        // Keep hidden tabs reachable when the pair and its tabs cannot fit.
        let rows = row_model(c, 5);
        assert_eq!(rows[3], SidebarRow::RosterOverflow);
        assert_eq!(rows[4], SidebarRow::NewWindow);
        // Below the minimum height there is no footer, and hidden tabs
        // stay reachable through the overflow row.
        let rows = row_model(c, 3);
        assert_eq!(
            rows,
            vec![
                SidebarRow::SpacesHeader,
                SidebarRow::RosterOverflow,
                SidebarRow::Blank,
            ]
        );
    }

    /// Agents keep a fixed share of the body: Sessions land on the same row
    /// whatever the population, and a calm panel keeps its header and a quiet
    /// placeholder (ADR-0112).
    #[test]
    fn row_model_fixes_the_agents_band_above_sessions() {
        // 2 agents + 1 tab in 10 rows: header, filter chips, two agent rows,
        // then blanks.
        assert_eq!(
            row_model(counts(2, 1, 0), 10),
            vec![
                SidebarRow::NeedsYouHeader,
                SidebarRow::AgentFilter,
                SidebarRow::NeedsYou(0),
                SidebarRow::NeedsYou(1),
                SidebarRow::Blank,
                SidebarRow::Blank,
                SidebarRow::Blank,
                SidebarRow::Blank,
                SidebarRow::Blank,
                SidebarRow::NewWindow,
            ]
        );
        // The Sessions header is at the same row for every population.
        let sessions_at = |n| {
            row_model(counts(n, 1, 2), 12)
                .iter()
                .position(|r| *r == SidebarRow::SpacesHeader)
                .expect("sessions laid")
        };
        for n in 0..=9 {
            assert_eq!(
                sessions_at(n),
                sessions_at(0),
                "population {n} moved Sessions"
            );
        }
        // ...and the panel is exactly the configured share of the body
        // (12 rows - 1 footer = 11).
        assert_eq!(sessions_at(3), agents_budget(DEFAULT_AGENTS_SHARE, 11));
        // A calm panel keeps its header and quiet placeholder; Sessions still
        // sits on the fixed line.
        let rows = row_model(counts(0, 1, 2), 12);
        assert_eq!(rows[0], SidebarRow::NeedsYouHeader);
        assert_eq!(rows[1], SidebarRow::AgentFilter);
        assert_eq!(rows[2], SidebarRow::NeedsYouEmpty);
        let sessions = sessions_at(0);
        assert_eq!(rows[sessions], SidebarRow::SpacesHeader);
        assert_eq!(rows[sessions + 1], SidebarRow::RosterHost(0));
        assert_eq!(rows[sessions + 2], SidebarRow::RosterEntry(0));
        assert_eq!(rows[sessions + 3], SidebarRow::RosterHost(1));
        assert_eq!(rows[sessions + 4], SidebarRow::RosterEntry(1));
        // Sessions sharing a machine share one header.
        let one_machine = SidebarCounts {
            host_starts: 1,
            ..counts(0, 1, 2)
        };
        let rows = row_model(one_machine, 12);
        assert_eq!(rows[2], SidebarRow::NeedsYouEmpty);
        let sessions = rows
            .iter()
            .position(|r| *r == SidebarRow::SpacesHeader)
            .expect("sessions");
        assert_eq!(rows[sessions + 1], SidebarRow::RosterHost(0));
        assert_eq!(rows[sessions + 2], SidebarRow::RosterEntry(0));
        assert_eq!(rows[sessions + 3], SidebarRow::RosterEntry(1));
        // A strip with nothing to show paints no panels at all.
        let rows = row_model(counts(0, 1, 0), 12);
        assert!(!rows.contains(&SidebarRow::SpacesHeader));
        assert!(!rows.contains(&SidebarRow::NeedsYouHeader));
    }

    /// One machine opens no segment; two or more open one each.
    #[test]
    fn a_lone_machine_paints_no_host_line() {
        assert_eq!(
            segment_starts([(false, "mini"), (false, "mini")].into_iter()),
            0
        );
        assert_eq!(
            segment_starts([(false, "mini"), (false, "mini"), (false, "xps")].into_iter()),
            0b101
        );
    }

    /// Agents overflow at the panel's capacity, not a population-based cap.
    #[test]
    fn agents_overflow_declares_what_it_dropped() {
        // A 15-row body at half reserves 8 rows: a header, the filter chips,
        // and six list rows (one of which becomes `+N more` when over
        // capacity).
        let rows = row_model(counts(9, 1, 0), 16);
        let listed = rows
            .iter()
            .filter(|r| matches!(r, SidebarRow::NeedsYou(_)))
            .count();
        assert_eq!(listed, 5, "{rows:?}");
        assert!(rows.contains(&SidebarRow::NeedsYouOverflow), "{rows:?}");
        // Exactly at capacity there is nothing to declare.
        let rows = row_model(counts(6, 1, 0), 16);
        assert!(!rows.contains(&SidebarRow::NeedsYouOverflow), "{rows:?}");
    }

    /// The panel is a fixed share of the body, so however many agents need a
    /// human Sessions always keeps their header at the same row.
    #[test]
    fn agents_band_is_fixed_and_never_starves_sessions() {
        for h in MIN_FOOTER_HEIGHT..24 {
            let rows = row_model(counts(20, 2, 3), h);
            let body = usize::from(h) - 1;
            assert_eq!(rows[0], SidebarRow::NeedsYouHeader, "h={h}");
            let band = agents_budget(DEFAULT_AGENTS_SHARE, body);
            assert_eq!(rows[band], SidebarRow::SpacesHeader, "h={h}");
            assert!(band < body, "Sessions keeps a row: h={h}");
        }
    }

    /// The invariants that must hold for every shape, since the allocator is
    /// what both the painter and the hit-tester read.
    #[test]
    fn row_model_invariants_hold_across_shapes() {
        for needs_you in [0usize, 1, 5, 9] {
            for windows in [0usize, 1, 4] {
                for roster in [0usize, 1, 7] {
                    for h in 0u16..26 {
                        let c = counts(needs_you, windows, roster);
                        let rows = row_model(c, h);
                        assert_eq!(rows.len(), usize::from(h), "{c:?} h={h}");

                        let footer = rows.contains(&SidebarRow::NewWindow);
                        assert_eq!(
                            footer,
                            h >= MIN_FOOTER_HEIGHT,
                            "footer presence tracks the height floor: {c:?} h={h}"
                        );

                        assert_model_items(c, &rows);
                    }
                }
            }
        }
    }

    fn assert_model_items(c: SidebarCounts, rows: &[SidebarRow]) {
        let mut agents = Vec::new();
        let mut sessions = Vec::new();
        for (y, row) in rows.iter().enumerate() {
            match row {
                SidebarRow::NeedsYou(j) => {
                    assert!(*j < c.needs_you);
                    agents.push(*j);
                }
                SidebarRow::RosterEntry(j) => {
                    assert!(*j < c.roster);
                    // A segment's header sits directly above its first
                    // session, and only there.
                    let header = y.checked_sub(1).and_then(|above| rows.get(above));
                    assert_eq!(
                        header == Some(&SidebarRow::RosterHost(*j)),
                        opens_segment(c, *j),
                        "entry {j} at y={y}"
                    );
                    sessions.push(*j);
                }
                SidebarRow::RosterHost(j) => {
                    assert!(opens_segment(c, *j));
                    assert_eq!(rows.get(y + 1), Some(&SidebarRow::RosterEntry(*j)));
                }
                SidebarRow::WindowName(i) => assert!(*i < c.windows),
                _ => {}
            }
        }
        assert_eq!(agents, (0..agents.len()).collect::<Vec<_>>());
        assert_eq!(sessions, (0..sessions.len()).collect::<Vec<_>>());
    }

    #[test]
    fn hit_test_maps_rows_to_targets() {
        let rect = r(0, 0, 20, 14);
        let c = SidebarCounts {
            active_session: Some(0),
            rule: SidebarRule::Trailing,
            agents_share: 0,
            ..counts(0, 2, 1)
        };
        assert_eq!(hit_test(rect, c, 3, 0), Some(SidebarHit::Sessions));
        assert_eq!(hit_test(rect, c, 3, 3), Some(SidebarHit::Window(0)));
        assert_eq!(hit_test(rect, c, 3, 4), Some(SidebarHit::Window(1)));
        // Padding rows miss.
        assert_eq!(hit_test(rect, c, 3, 5), None);
        // Footer row.
        assert_eq!(hit_test(rect, c, 3, 13), Some(SidebarHit::NewWindow));
    }

    /// Agent and session rows retain their own destinations under overflow.
    #[test]
    fn hit_test_maps_the_new_zones() {
        let rect = r(0, 0, 20, 10);
        // 2 agents + 1 tab: rows 0 header, 1 filter chips, 2-3 agents, then
        // blank.
        let c = counts(2, 1, 0);
        assert_eq!(hit_test(rect, c, 3, 0), Some(SidebarHit::Fleet));
        assert_eq!(
            hit_test(rect, c, 3, 1),
            Some(SidebarHit::AgentFilter(FilterChip::Scope(AgentScope::All)))
        );
        // Each chip is its own target; the space between chips is inert.
        assert_eq!(
            hit_test(rect, c, 6, 1),
            Some(SidebarHit::AgentFilter(FilterChip::Scope(
                AgentScope::Session
            )))
        );
        assert_eq!(
            hit_test(rect, c, 11, 1),
            Some(SidebarHit::AgentFilter(FilterChip::Scope(
                AgentScope::Attention
            )))
        );
        assert_eq!(hit_test(rect, c, 4, 1), None, "between chips");
        assert_eq!(hit_test(rect, c, 3, 2), Some(SidebarHit::NeedsYou(0)));
        assert_eq!(hit_test(rect, c, 3, 3), Some(SidebarHit::NeedsYou(1)));
        assert_eq!(hit_test(rect, c, 3, 4), None, "blank row");
        // The `@` chip only when the agents span machines.
        assert_eq!(hit_test(rect, c, 16, 1), None);
        let hosts = SidebarCounts {
            agent_host_chip: true,
            ..c
        };
        assert_eq!(
            hit_test(rect, hosts, 16, 1),
            Some(SidebarHit::AgentFilter(FilterChip::Host))
        );

        // Roster rows, below the fixed Agents panel (header + quiet
        // placeholder + padding in a 12-row strip).
        let tall = Rect { h: 12, ..rect };
        let c = counts(0, 1, 2);
        assert_eq!(hit_test(tall, c, 3, 0), Some(SidebarHit::Fleet));
        assert_eq!(hit_test(tall, c, 3, 2), None, "the quiet placeholder");
        let sessions = agents_budget(c.agents_share, usize::from(tall.h) - 1);
        assert_eq!(
            hit_test(tall, c, 3, u16::try_from(sessions).unwrap()),
            Some(SidebarHit::Sessions)
        );
        assert_eq!(
            hit_test(tall, c, 3, u16::try_from(sessions + 1).unwrap()),
            Some(SidebarHit::Roster(0))
        );
        assert_eq!(
            hit_test(tall, c, 3, u16::try_from(sessions + 2).unwrap()),
            Some(SidebarHit::Roster(0))
        );
        assert_eq!(
            hit_test(tall, c, 3, u16::try_from(sessions + 3).unwrap()),
            Some(SidebarHit::Roster(1))
        );
        assert_eq!(
            hit_test(tall, c, 3, u16::try_from(sessions + 4).unwrap()),
            Some(SidebarHit::Roster(1))
        );

        // Overflow rows open the dashboard — the surface that has what the
        // strip had to drop.
        let big = Rect { h: 16, ..rect };
        let c = counts(9, 1, 0);
        let model = row_model(c, big.h);
        let row = model
            .iter()
            .position(|r| matches!(r, SidebarRow::NeedsYouOverflow))
            .expect("overflow allocated");
        assert_eq!(
            hit_test(big, c, 3, u16::try_from(row).unwrap()),
            Some(SidebarHit::Fleet)
        );
        let c = counts(0, 0, 9);
        let model = row_model(c, tall.h);
        let row = model
            .iter()
            .position(|r| *r == SidebarRow::RosterOverflow)
            .expect("overflow");
        assert_eq!(
            hit_test(tall, c, 3, u16::try_from(row).unwrap()),
            Some(SidebarHit::Sessions)
        );
    }

    /// The bottom corner cell is the collapse chevron — the
    /// only interactive cell on the separator column.
    #[test]
    fn hit_test_resolves_the_collapse_corner() {
        let rect = r(0, 0, 20, 8);
        let c = counts(0, 1, 0);
        assert_eq!(hit_test(rect, c, 19, 7), Some(SidebarHit::Collapse));
        // The rest of the separator column stays inert.
        assert_eq!(hit_test(rect, c, 19, 6), None);
        assert_eq!(hit_test(rect, c, 19, 0), None);
        // No footer (short strip) => no chevron target.
        let short = r(0, 0, 20, 3);
        assert_eq!(hit_test(short, c, 19, 2), None);
    }

    #[test]
    fn hit_test_respects_the_rect_origin_and_separator() {
        // A strip at x=60, y=2; agent 0 is at local row 2, under the
        // header and the filter chips.
        let rect = r(60, 2, 20, 8);
        let c = counts(1, 0, 0);
        assert_eq!(hit_test(rect, c, 60, 4), Some(SidebarHit::NeedsYou(0)));
        assert_eq!(
            hit_test(rect, c, 62, 3),
            Some(SidebarHit::AgentFilter(FilterChip::Scope(AgentScope::All)))
        );
        // The separator column (last column of the strip) is not a target
        // outside the chevron corner.
        assert_eq!(hit_test(rect, c, 79, 0), None);
        // Outside the strip entirely.
        assert_eq!(hit_test(rect, c, 59, 1), None);
        assert_eq!(hit_test(rect, c, 80, 1), None);
        assert_eq!(hit_test(rect, c, 60, 10), None);
        // Degenerate rects never hit.
        assert_eq!(hit_test(r(0, 0, 0, 0), c, 0, 0), None);
    }

    /// The filter row paints its chips, and its first cell under the probe
    /// column is the `all` chip.
    fn assert_filter_row(text: &str, hit: Option<SidebarHit>) {
        assert!(text.contains(AgentScope::All.label()), "{text:?}");
        assert_eq!(
            hit,
            Some(SidebarHit::AgentFilter(FilterChip::Scope(AgentScope::All)))
        );
    }

    /// Paint and hit-test derive from one row model: every row the painter
    /// fills with a window label hit-tests to that window, agent rows
    /// hit-test to their windows, and the footer row hit-tests to its
    /// affordance.
    #[test]
    fn paint_and_hit_test_agree_row_for_row() {
        let rect = r(0, 0, 26, 22);
        let windows = vec![
            win_branch("alpha", true, "main"),
            win("beta", false),
            win_branch("gamma", false, "dev"),
        ];
        let agents = vec![
            agent(1, "beta", "claude", AgentMetaState::Blocked),
            agent(2, "gamma", "codex", AgentMetaState::Done),
        ];
        let peers = vec![roster("delta", 1, 0, 0), roster("epsilon", 0, 1, 0)];
        let sections = vec![plugin_section("Builds", 2, &["lint", "test", "docs"])];
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(windows.clone());
        p.set_agents(agents.clone());
        p.set_roster(peers.clone());
        p.set_plugin_sections(sections.clone());
        let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
        let c = p.counts();
        for (y, row) in row_model(c, rect.h).iter().enumerate() {
            let y16 = u16::try_from(y).expect("row fits u16");
            let hit = hit_test(rect, c, 2, y16);
            // Exhaustive on purpose: a new SidebarRow variant must fail to
            // compile here rather than slip through a catch-all with no
            // paint/click agreement check of its own.
            match row {
                SidebarRow::NeedsYouHeader => {
                    assert!(row_text(&buf, rect, y16).contains(NEEDS_YOU_HEADER));
                    assert_eq!(hit, Some(SidebarHit::Fleet));
                }
                SidebarRow::AgentFilter => assert_filter_row(&row_text(&buf, rect, y16), hit),
                SidebarRow::SpacesHeader => {
                    assert!(row_text(&buf, rect, y16).contains(SPACES_HEADER));
                    assert_eq!(hit, Some(SidebarHit::Sessions));
                }
                SidebarRow::WindowName(i) => {
                    assert!(row_text(&buf, rect, y16).contains(&windows[*i].name));
                    assert_eq!(hit, Some(SidebarHit::Window(*i)));
                }
                SidebarRow::NeedsYou(j) => {
                    assert!(
                        [&agents[*j].window_name, &agents[*j].name]
                            .iter()
                            .all(|label| row_text(&buf, rect, y16).contains(label.as_str()))
                    );
                    assert_eq!(hit, Some(SidebarHit::NeedsYou(*j)));
                }
                SidebarRow::RosterEntry(j) => {
                    assert!(row_text(&buf, rect, y16).contains(&peers[*j].name));
                    assert_eq!(hit, Some(SidebarHit::Roster(*j)));
                }
                SidebarRow::RosterHost(j) => {
                    assert!(row_text(&buf, rect, y16).contains(&peers[*j].host));
                    assert_eq!(hit, Some(SidebarHit::Roster(*j)));
                }
                SidebarRow::NeedsYouOverflow => {
                    assert!(row_text(&buf, rect, y16).contains("more"));
                    assert_eq!(hit, Some(SidebarHit::Fleet));
                }
                SidebarRow::RosterOverflow => {
                    assert_eq!(hit, Some(SidebarHit::Sessions));
                }
                SidebarRow::Blank | SidebarRow::NeedsYouEmpty => {
                    assert_eq!(hit, None);
                }
                SidebarRow::NewWindow => {
                    assert!(row_text(&buf, rect, y16).contains(NEW_LABEL));
                    assert_eq!(hit, Some(SidebarHit::NewWindow));
                }
                SidebarRow::PluginHeader(_)
                | SidebarRow::PluginEntry(..)
                | SidebarRow::PluginOverflow(_)
                | SidebarRow::PluginEmpty(_) => {
                    assert_plugin_row(*row, &row_text(&buf, rect, y16), hit, &sections);
                }
            }
        }
        assert!(
            row_model(c, rect.h).contains(&SidebarRow::PluginOverflow(0)),
            "the fixture overflows its section"
        );
    }

    /// The plugin half of [`paint_and_hit_test_agree_row_for_row`].
    fn assert_plugin_row(
        row: SidebarRow,
        text: &str,
        hit: Option<SidebarHit>,
        sections: &[PluginSection],
    ) {
        match row {
            SidebarRow::PluginHeader(s) => {
                assert!(text.contains(&sections[s].title));
                assert_eq!(hit, None);
            }
            SidebarRow::PluginEntry(s, j) => {
                assert!(text.contains(&sections[s].entries[j].text));
                assert_eq!(hit, Some(SidebarHit::Plugin(s, j)));
            }
            SidebarRow::PluginOverflow(_) => {
                assert!(text.contains("+2"));
                assert_eq!(hit, None);
            }
            _ => assert_eq!(hit, None),
        }
    }

    fn plugin_section(title: &str, rows: u8, texts: &[&str]) -> PluginSection {
        PluginSection {
            title: title.to_owned(),
            rows,
            entries: texts
                .iter()
                .enumerate()
                .map(
                    |(i, text)| crate::render::chrome::sidebar_sections::PluginSectionRow {
                        text: (*text).to_owned(),
                        window: i,
                        pane: 0,
                    },
                )
                .collect(),
        }
    }

    /// ADR-0148: a plugin section is a fixed band between Agents and
    /// Sessions; its population never moves the Sessions header, an empty
    /// section shows the quiet dash, and a strip too short for it keeps the
    /// two core panels instead.
    #[test]
    fn plugin_sections_hold_a_fixed_band_between_agents_and_sessions() {
        let rect = r(0, 0, 28, 24);
        let mut p = SidebarPainter::new(Theme::default());
        p.set_roster(vec![active_roster()]);
        p.set_plugin_sections(vec![plugin_section("Builds", 3, &[])]);
        let model = row_model(p.counts(), rect.h);
        let sessions_at = |m: &[SidebarRow]| m.iter().position(|r| *r == SidebarRow::SpacesHeader);
        let empty_sessions = sessions_at(&model);
        let header = model
            .iter()
            .position(|r| *r == SidebarRow::PluginHeader(0))
            .expect("section laid");
        assert!(Some(header) < empty_sessions);
        assert_eq!(model[header + 1], SidebarRow::PluginEmpty(0));
        let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
        assert!(row_text(&buf, rect, u16::try_from(header).expect("row")).contains("Builds"));

        p.set_plugin_sections(vec![plugin_section(
            "Builds",
            3,
            &["a", "b", "c", "d", "e"],
        )]);
        let full = row_model(p.counts(), rect.h);
        assert_eq!(
            sessions_at(&full),
            empty_sessions,
            "population never moves Sessions"
        );
        assert_eq!(
            &full[header..header + 4],
            &[
                SidebarRow::PluginHeader(0),
                SidebarRow::PluginEntry(0, 0),
                SidebarRow::PluginEntry(0, 1),
                SidebarRow::PluginOverflow(0),
            ]
        );
        let targets = p.click_targets();
        assert_eq!(targets.plugin[0][1], (1, 0));
        assert_eq!(
            hit_test(
                rect,
                targets.counts,
                2,
                u16::try_from(header + 2).expect("row")
            ),
            Some(SidebarHit::Plugin(0, 1))
        );

        // Too short for the band plus the core minimum: no section at all.
        let short = row_model(p.counts(), 12);
        assert!(
            !short
                .iter()
                .any(|r| matches!(r, SidebarRow::PluginHeader(_)))
        );
        assert!(short.contains(&SidebarRow::SpacesHeader));
    }

    /// The rail and any divider reaching the strip tee into the separator on
    /// the pane-facing side, so the grid and the strip read as one frame.
    #[test]
    fn the_rail_junction_tees_into_the_panes() {
        let p = SidebarPainter::new(Theme::default());
        let rect = r(0, 0, 20, 10);
        let left = p.compose_buffer(rect, SidebarRule::Trailing, &[1]);
        assert_eq!(left[(19, 1)].symbol(), "├");
        assert_eq!(left[(19, 2)].symbol(), "│");
        let right = p.compose_buffer(rect, SidebarRule::Leading, &[1]);
        assert_eq!(right[(0, 1)].symbol(), "┤");
        let none = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
        assert_eq!(none[(19, 1)].symbol(), "│");
        // A horizontal divider reaching the strip tees in too.
        let both = p.compose_buffer(rect, SidebarRule::Trailing, &[1, 6]);
        assert_eq!(both[(19, 1)].symbol(), "├");
        assert_eq!(both[(19, 6)].symbol(), "├");
        assert_eq!(both[(19, 5)].symbol(), "│");
    }

    /// Headers carry their count and every row's secondary text lands on
    /// the same right edge, so the strip reads as a table.
    #[test]
    fn headers_count_and_secondary_text_share_one_right_edge() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_agents(vec![
            agent(0, "editor", "claude", AgentMetaState::Blocked),
            agent(1, "build", "codex", AgentMetaState::Done),
        ]);
        p.set_roster(vec![active_roster()]);
        p.set_windows(vec![win_branch("editor", true, "main")]);
        let rect = r(0, 0, 30, 16);
        let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
        let rows: Vec<String> = (0..rect.h).map(|y| row_text(&buf, rect, y)).collect();
        assert!(
            rows[0].starts_with(" Agents") && rows[0].ends_with("2 "),
            "{rows:?}"
        );
        assert!(rows[2].ends_with("claude "), "{rows:?}");
        assert!(rows[3].ends_with("codex "), "{rows:?}");
        let window = rows
            .iter()
            .find(|row| row.contains("editor") && row.contains("main"))
            .expect("window row");
        assert!(window.ends_with("main "), "branch flush right: {window:?}");
    }
}
