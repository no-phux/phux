//! Fixed-area Agents and Sessions sidebar painter.
//!
//! Agents occupy the upper half of the body in caller-supplied stable order;
//! lifecycle changes update their badges in place. Sessions occupy the lower
//! half, each with a secondary host line. The active session expands compact
//! window names directly beneath it. Neither area's position depends on its
//! population, and empty areas keep their headers and placeholders.
//!
//! The strip's last two rows hold New window plus a shared Commands / Settings
//! action row (phux-fce4), bottom-anchored, with a collapse chevron in the bottom
//! corner cell (phux-foz.9; clicking it runs `toggle-sidebar`).
//! [`hit_test`] maps a mouse position back onto the same row model so
//! clicks land exactly where the paint says they should. A vertical rule
//! on the strip's last column separates it from the panes. The
//! reservation + placement is owned by the driver; this type just paints
//! into the `Rect` it is handed and caches the last paint so an unchanged
//! repaint emits nothing — the same incremental discipline as the status
//! bar.

use std::io::{self, Write};

use phux_config::widget::WindowInfo;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect as RataRect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::layout::Rect;
use crate::render::Theme;
use crate::render::overlay::HardcodedBinding;
use crate::render::{clip_text, display_width};
use phux_client::agent_meta::AgentMetaState;

/// Label of the "create" affordance row (phux-fce4).
///
/// Clicking it runs the `new-window` action — the sidebar lists windows,
/// so `+ new window` creates one.
pub const NEW_LABEL: &str = "+ new window";
/// Label of the "menu" affordance row (phux-fce4).
///
/// Clicking it opens the command palette — the one menu that covers
/// window, session, and plugin actions (`new-session` included) through
/// the action registry.
pub const MENU_LABEL: &str = "= commands";
/// Label of the Settings affordance on the shared footer row.
pub const SETTINGS_LABEL: &str = "S settings";
/// Gap between the two independently clickable actions on the footer's menu row.
const FOOTER_ACTION_GAP: &str = "  ";
/// Agents header. The legacy API name now refers to the full agent list.
pub const NEEDS_YOU_HEADER: &str = "Agents";
/// Sessions header, including the current session.
pub const SPACES_HEADER: &str = "Sessions";
/// Quiet placeholders keep both fixed areas recognizable.
pub const AGENTS_EMPTY: &str = "none running yet";
/// Placeholder when no sessions are available.
pub const SESSIONS_EMPTY: &str = "no sessions";
/// Label of a truncated area's overflow row.
pub const OVERFLOW_LABEL: &str = "more";
/// The collapse chevron painted in the strip's bottom corner
/// (phux-foz.9). Clicking it runs `toggle-sidebar`.
pub const COLLAPSE_GLYPH: &str = "‹";

/// The sidebar's click-target table for handler-adjacency tests.
/// `Mouse & menus` section (phux-i0e8.10.3).
///
/// COLOCATED with [`hit_test`]
/// and the row model it reads, and REUSING the affordance-label consts
/// above so a rename breaks the help text visibly instead of letting it
/// rot. The `help_table_matches_hit_targets` adjacency test drives each
/// advertised click through the real [`hit_test`].
pub static HELP_BINDINGS: &[HardcodedBinding] = &[
    HardcodedBinding {
        chord: "click",
        action: "select the clicked window (sidebar row)",
    },
    HardcodedBinding {
        chord: NEEDS_YOU_HEADER,
        action: "jump to the clicked agent (sidebar click)",
    },
    HardcodedBinding {
        chord: SPACES_HEADER,
        action: "switch to that session (sidebar roster click)",
    },
    HardcodedBinding {
        chord: OVERFLOW_LABEL,
        action: "open agents or sessions for that area's overflow (sidebar click)",
    },
    HardcodedBinding {
        chord: NEW_LABEL,
        action: "create a window (sidebar click)",
    },
    HardcodedBinding {
        chord: MENU_LABEL,
        action: "open the command palette (sidebar click)",
    },
    HardcodedBinding {
        chord: SETTINGS_LABEL,
        action: "open Settings (sidebar click)",
    },
    HardcodedBinding {
        chord: COLLAPSE_GLYPH,
        action: "collapse the sidebar (bottom-corner click)",
    },
];

/// Minimum strip height (rows) at which the footer affordances render.
/// Below this every row goes to the section body — a 2–3 row strip
/// showing only chrome and no windows would be useless.
const MIN_FOOTER_HEIGHT: u16 = 4;

/// Whole-cell spacing tokens, shared by every sidebar row (docs/experience.md).
const GUTTER: u16 = 1;
const ICON_COLUMNS: usize = 2;

/// One agent-running pane, as the sidebar's `agents` section renders it
/// (phux-foz.9).
///
/// Built by the driver from the ADR-0040 `phux.agent/v1` record when the
/// pane declares one, else from the OSC-title identity heuristic
/// ([`phux_client::agent_meta::agent_name_from_title`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEntry {
    /// The session holding the agent's pane, or `None` for the session this
    /// client is attached to (phux-k0cw).
    ///
    /// `None` is what keeps a local row cheap: it commits `select-window`,
    /// which moves client-local focus and nothing else. `Some(name)` commits
    /// `switch-session { name, window, pane }` — a real re-attach — so the
    /// two are deliberately different types of click, not the same click with
    /// a different argument.
    pub session: Option<String>,
    /// Index of the window holding the agent's pane (its `select-window`
    /// index) — clicking the row jumps there.
    pub window: usize,
    /// The window's stored name, herdr's "workspace" column on the row.
    pub window_name: String,
    /// The pane's DFS leaf ordinal inside its window, when known
    /// (phux-k0cw).
    ///
    /// Only a cross-session commit needs it: `switch-session` can select the
    /// pane as well as the window, so an agent row lands the user on the pane
    /// that wants them rather than on its window's remembered focus. `None`
    /// for a local row, which never needs it.
    pub pane: Option<usize>,
    /// Agent display name, e.g. `claude` or `merge-queue-w5`.
    pub name: String,
    /// Lifecycle state; picks the row's glyph + color.
    pub state: AgentMetaState,
    /// `true` when the agent is waiting on a human (declared high
    /// attention, or the pane's ADR-0035 asked flag).
    pub attention: bool,
    /// `true` once the user has visited this agent's pane since its last
    /// state change. Drives the "finished but unreviewed" tier of
    /// [`attention_rank`] and the row's glyph: a `done` agent you have not
    /// looked at yet reads as "look at me"; one you have is quiet.
    ///
    /// A real display input, so it belongs in the struct (which is the
    /// [`SidebarPainter`]'s content-cache key). The *timestamp* of the last
    /// change deliberately does NOT: a per-frame-varying value in here would
    /// miss the cache every frame and repaint the strip forever. The driver
    /// keeps `last_change` in a side map. The painter preserves input order.
    pub seen: bool,
}

/// Where an agent row sits on the attention ladder — higher demands a human
/// sooner.
///
/// This severity scale drives badges and session summaries. It does not
/// determine the sidebar's stable display order.
///
/// ```text
/// blocked  >  done AND !seen  >  working  >  done/idle AND seen  >  unknown
/// ```
///
/// An unreviewed result has a more emphatic badge than ongoing work. Visiting
/// the pane (`seen`) quiets that badge without changing the row's position.
///
/// `attention` (a declared high-attention record, or the ADR-0035 asked flag)
/// pins the row to the top rung regardless of state: an agent that has
/// explicitly asked for a human IS blocked on one.
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

/// One session, including the current session, with explicit serving-host
/// identity. An unreachable host may contribute an unselectable placeholder.
///
/// The counts are carried rather than reduced to a single worst-state colour
/// because a dot says *what* and a count says *how much*: `!1 *2` is a
/// different morning than `!1`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionRosterEntry {
    /// The session's name — also what a click commits as
    /// `switch-session { name }`.
    pub name: String,
    /// Display label of the serving host, supplied by the projection.
    pub host: String,
    /// Whether this is the session currently attached to the client.
    pub active: bool,
    /// Satellite name for the `switch-session` host argument, if required.
    pub route_host: Option<String>,
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
}

impl SessionRosterEntry {
    /// The session's own rung on the attention ladder: the highest rung any
    /// of its panes occupies.
    ///
    /// Uses the same rungs as [`attention_rank`] so session summaries and
    /// individual agent badges agree about severity.
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

/// The counts the strip's shape is derived from (phux-k0cw).
///
/// [`row_model`] takes this rather than the projections themselves, which is
/// what lets the input dispatcher hit-test a click without rebuilding the
/// window/agent/roster lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SidebarCounts {
    /// Full Agents list, in stable input order (legacy field name).
    pub needs_you: usize,
    /// Windows in the focused session, nested beneath its roster entry.
    pub windows: usize,
    /// All session entries, including the current session.
    pub roster: usize,
    /// Roster index whose windows expand, or None when there is no active entry.
    pub active_session: Option<usize>,
}

/// One row of the strip, top to bottom. Both the painter and [`hit_test`]
/// derive from this single model, so paint and click targets cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarRow {
    /// Fixed Agents header (legacy name).
    NeedsYouHeader,
    /// Agent entry `j`'s row (glyph + session/window + `state - name`).
    NeedsYou(usize),
    /// Agents overflow opens the fleet dashboard.
    NeedsYouOverflow,
    /// Quiet placeholder beneath the Agents header.
    AgentsEmpty,
    /// Window `i`'s name row.
    WindowName(usize),
    /// Fixed Sessions header (legacy name).
    SpacesHeader,
    /// Roster entry `j`'s row (dot + session name + state histogram).
    RosterEntry(usize),
    /// Secondary serving-host identity; shares the session name's target.
    RosterHost(usize),
    /// Quiet placeholder beneath the Sessions header.
    SessionsEmpty,
    /// Hidden sessions/windows; opens the session picker.
    RosterOverflow,
    /// Unused padding (section gap, or fill above the footer).
    Blank,
    /// The `+ new` affordance (create a window).
    NewWindow,
    /// The shared `= commands  S settings` affordance row.
    Menu,
}

/// The interactive target a mouse position resolves to (phux-fce4).
///
/// Deliberately INDEX-based rather than carrying resolved names, so the enum
/// stays `Copy` and the row model remains derivable from counts alone. The
/// caller resolves an index against [`SidebarTargets`] at commit time — see
/// that type for why the resolution must re-check the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarHit {
    /// A nested window row selects window `i` (its `select-window` index).
    Window(usize),
    /// Agent row `j` (legacy variant name).
    NeedsYou(usize),
    /// Session name or host row `j`.
    Roster(usize),
    /// Agents overflow opens the agent-fleet dashboard.
    Fleet,
    /// Sessions overflow opens the session picker.
    Sessions,
    /// The `+ new` affordance.
    NewWindow,
    /// The `= menu` affordance.
    Menu,
    /// The `S settings` affordance.
    Settings,
    /// The collapse chevron in the bottom corner (phux-foz.9) —
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
        /// The peer session's name.
        name: String,
        /// Window index within that session.
        window: usize,
        /// Pane ordinal within that window.
        pane: usize,
    },
}

/// The click-resolution table for one painted frame (phux-k0cw).
///
/// [`SidebarHit`] carries an index; this turns the index back into an
/// action. It is snapshotted per paint, which opens a staleness window: list
/// membership can change, so an index resolved against a newer table could
/// send the user somewhere they did not click. A
/// same-session `select-window` is forgiving of that; a `switch-session`
/// re-attach is not, which is why the dispatcher commits the resolved NAME
/// rather than re-deriving it.
#[derive(Debug, Clone, Default)]
pub struct SidebarTargets {
    /// The counts the frame was painted from — the same ones [`hit_test`]
    /// must be given, so a click resolves against the shape it landed on.
    pub counts: SidebarCounts,
    /// Agent targets, in stable display order.
    pub needs_you: Vec<SidebarTarget>,
    /// Session destinations in display order; placeholders have no target.
    pub roster: Vec<Option<SessionRosterTarget>>,
}

/// Explicit session identity and optional satellite route for a roster click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRosterTarget {
    /// Session name, resolved against the painted frame.
    pub name: String,
    /// Satellite name passed to `switch-session`, if required.
    pub host: Option<String>,
}

/// Fixed half-height areas, independent of population.
///
/// The odd body row goes
/// to Sessions. A one-row viewport provides session navigation; otherwise both
/// headers persist. Name/host pairs are indivisible, including under overflow.
#[must_use]
pub fn row_model(counts: SidebarCounts, h: u16) -> Vec<SidebarRow> {
    let h = usize::from(h);
    let footer = if h >= usize::from(MIN_FOOTER_HEIGHT) {
        2
    } else {
        0
    };
    let body = h - footer;
    let mut rows = Vec::with_capacity(h);

    if body == 1 {
        rows.push(SidebarRow::RosterOverflow);
    } else {
        let agents = body / 2;
        push_agents(&mut rows, counts.needs_you, agents);
        rows.resize(agents, SidebarRow::Blank);
        push_sessions(&mut rows, counts, body - agents);
        rows.resize(body, SidebarRow::Blank);
    }
    if footer == 2 {
        rows.push(SidebarRow::NewWindow);
        rows.push(SidebarRow::Menu);
    }
    rows
}

/// Keep overflow reachable even when only one content row fits.
fn push_agents(rows: &mut Vec<SidebarRow>, count: usize, budget: usize) {
    if budget == 0 {
        return;
    }
    rows.push(SidebarRow::NeedsYouHeader);
    let room = budget - 1;
    if room == 0 {
        return;
    }
    if count == 0 {
        rows.push(SidebarRow::AgentsEmpty);
        return;
    }
    let overflow = count > room;
    let shown = count.min(room - usize::from(overflow));
    rows.extend((0..shown).map(SidebarRow::NeedsYou));
    if overflow {
        rows.push(SidebarRow::NeedsYouOverflow);
    }
}

/// Append the fixed Sessions area with complete host pairs and nested windows.
fn push_sessions(rows: &mut Vec<SidebarRow>, counts: SidebarCounts, budget: usize) {
    if budget == 0 {
        return;
    }
    let end = rows.len() + budget;
    rows.push(SidebarRow::SpacesHeader);
    let room = budget - 1;
    if room == 0 {
        return;
    }
    if counts.roster == 0 {
        rows.push(SidebarRow::SessionsEmpty);
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
    counts.roster.saturating_mul(2).saturating_add(windows)
}

/// Limit iteration by viewport capacity, even for extremely large counts.
fn push_session_entries(rows: &mut Vec<SidebarRow>, counts: SidebarCounts, limit: usize) {
    for j in 0..counts.roster {
        if limit.saturating_sub(rows.len()) < 2 {
            break;
        }
        rows.extend([SidebarRow::RosterEntry(j), SidebarRow::RosterHost(j)]);
        if counts.active_session == Some(j) {
            let shown = counts.windows.min(limit - rows.len());
            rows.extend((0..shown).map(SidebarRow::WindowName));
        }
    }
}

/// Resolve an outer-viewport mouse cell to a sidebar target.
///
/// `None` when it misses the strip (or lands on the separator column or a
/// blank row). Section headers route to their full management views. `counts`
/// must be the same shape the painter was fed, so a click resolves against the
/// frame it landed on. The bottom
/// corner cell — on the separator column, which is otherwise never a
/// target — is the collapse chevron (phux-foz.9).
#[must_use]
pub fn hit_test(rect: Rect, counts: SidebarCounts, x: u16, y: u16) -> Option<SidebarHit> {
    let local_x = x.checked_sub(rect.x)?;
    let local_y = y.checked_sub(rect.y)?;
    if local_x >= rect.w || local_y >= rect.h {
        return None;
    }
    // The bottom corner cell is the collapse chevron whenever the footer
    // renders (same condition the painter uses).
    if collapse_visible(rect) && local_x == rect.w - 1 && local_y == rect.h - 1 {
        return Some(SidebarHit::Collapse);
    }
    // The rest of the last column is the separator rule, not a target.
    if local_x >= rect.w.saturating_sub(1) {
        return None;
    }
    let row = *row_model(counts, rect.h).get(usize::from(local_y))?;
    if row == SidebarRow::Menu {
        return footer_action_hit(local_x, rect.w);
    }
    row_hit(row)
}

/// Resolve the two actions that share the final footer row against the exact
/// text columns used by [`SidebarPainter::footer_actions_line`]. On narrow
/// strips only the visible Commands label remains interactive.
fn footer_action_hit(local_x: u16, width: u16) -> Option<SidebarHit> {
    let text_w = usize::from(width.saturating_sub(1 + GUTTER * 2));
    let content_x = usize::from(local_x.checked_sub(GUTTER)?);
    let menu_w = display_width(MENU_LABEL).min(text_w);
    if content_x < menu_w {
        return Some(SidebarHit::Menu);
    }

    let settings_start = display_width(MENU_LABEL) + display_width(FOOTER_ACTION_GAP);
    let settings_end = settings_start + display_width(SETTINGS_LABEL);
    (settings_end <= text_w && (settings_start..settings_end).contains(&content_x))
        .then_some(SidebarHit::Settings)
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
        SidebarRow::Menu
        | SidebarRow::AgentsEmpty
        | SidebarRow::SessionsEmpty
        | SidebarRow::Blank => None,
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
    needs_you: Vec<AgentEntry>,
    roster: Vec<SessionRosterEntry>,
    theme: Theme,
    /// Last successfully emitted cells. Keep only one projection copy and
    /// compare rendered rows so hidden/truncated changes emit no bytes.
    last: Option<(Rect, Buffer)>,
    dirty: bool,
}

impl SidebarPainter {
    /// A painter styled by `theme`, initially showing no windows.
    #[must_use]
    pub const fn new(theme: Theme) -> Self {
        Self {
            windows: Vec::new(),
            needs_you: Vec::new(),
            roster: Vec::new(),
            theme,
            last: None,
            dirty: true,
        }
    }

    /// Replace the window list (driver calls this from the same
    /// `window_infos` snapshot that feeds the status-bar tab strip).
    /// Returns `true` if the list actually changed, so a caller with no
    /// other paint trigger (the agent-event chrome path) can gate a repaint
    /// on it; the paint cache below makes an unchanged repaint free either
    /// way.
    pub fn set_windows(&mut self, windows: Vec<WindowInfo>) -> bool {
        if self.windows == windows {
            return false;
        }
        self.windows = windows;
        self.dirty = true;
        true
    }

    /// Replace the full Agents list. The legacy name is retained for callers;
    /// entries are never sorted or filtered here, including idle/done agents.
    /// Same change-report contract as [`Self::set_windows`].
    pub fn set_needs_you(&mut self, needs_you: Vec<AgentEntry>) -> bool {
        if self.needs_you == needs_you {
            return false;
        }
        self.needs_you = needs_you;
        self.dirty = true;
        true
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

    /// The counts [`row_model`] and [`hit_test`] derive the strip's shape
    /// from.
    #[must_use]
    pub fn counts(&self) -> SidebarCounts {
        SidebarCounts {
            needs_you: self.needs_you.len(),
            windows: self.windows.len(),
            roster: self.roster.len(),
            active_session: self.roster.iter().position(|s| s.active),
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
                .map(|e| match (&e.session, e.pane) {
                    (Some(name), Some(pane)) => SidebarTarget::Session {
                        name: name.clone(),
                        window: e.window,
                        pane,
                    },
                    // A foreign row with no pane ordinal still switches
                    // sessions; it just lands on the session's remembered
                    // focus rather than the pane that wants you.
                    (Some(name), None) => SidebarTarget::Session {
                        name: name.clone(),
                        window: e.window,
                        pane: 0,
                    },
                    (None, _) => SidebarTarget::Window(e.window),
                })
                .collect(),
            roster: self
                .roster
                .iter()
                .map(|s| {
                    s.selectable.then(|| SessionRosterTarget {
                        name: s.name.clone(),
                        host: s.route_host.clone(),
                    })
                })
                .collect(),
        }
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
        let buf = self.compose(rect);
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

    /// Compose the strip into a `rect`-sized ratatui [`Buffer`] (origin
    /// `(0, 0)`), for the structured `snapshot --rendered` compositor
    /// (phux-l5xa / phux-4h5a). The VT [`Self::paint`] path uses the same
    /// `compose` step internally, so the cells match a live paint.
    #[must_use]
    pub fn compose_buffer(&self, rect: Rect) -> Buffer {
        self.compose(rect)
    }

    /// Render a muted section header.
    fn header_line(&self, label: &str, text_w: u16) -> Line<'static> {
        Line::from(Span::styled(
            truncate(label, usize::from(text_w)),
            Style::default()
                .fg(self.theme.sidebar_section)
                .add_modifier(Modifier::BOLD),
        ))
    }

    /// Render a section's empty-state placeholder (phux-foz.13): the label
    /// nested one indent under the header, dim + italic so it reads as a
    /// quiet "nothing here yet" rather than a real, selectable row.
    fn empty_line(&self, label: &str, text_w: u16) -> Line<'static> {
        let label = truncate(label, usize::from(text_w).saturating_sub(2));
        Line::from(Span::styled(
            format!("  {label}"),
            Style::default()
                .fg(self.theme.dim)
                .add_modifier(Modifier::ITALIC),
        ))
    }

    /// Render one window's name row: a status dot + the bold label.
    fn name_line(&self, w: &WindowInfo, text_w: u16) -> Line<'static> {
        // The dot carries status: filled + accent for the active window,
        // hollow + dim otherwise, attention amber when the window is
        // waiting on a human (ADR-0035).
        let (dot, dot_color) = match (w.attention, w.active) {
            (true, _) => ("●", self.theme.attention),
            (false, true) => ("●", self.theme.accent),
            (false, false) => ("○", self.theme.dim),
        };
        // phux-foz.1: reserve 2 cells for the ` !` attention
        // suffix so a long label can't push it off the strip.
        let label_w = usize::from(text_w)
            .saturating_sub(4) // nested indent + dot + space
            .saturating_sub(if w.attention { 2 } else { 0 });
        let label = truncate(&w.name, label_w);
        let branch = fitting_branch(w, label_w.saturating_sub(display_width(&label)));
        let style = if w.active {
            Style::default()
                .fg(self.theme.selection_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.theme.text)
        };
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(format!("{dot} "), Style::default().fg(dot_color)),
            Span::styled(label, style),
        ];
        // phux-foz.1: a window holding a pane that asked for a
        // human answer (ADR-0035) gets a themed `!` marker.
        if w.attention {
            spans.push(Span::styled(
                " !",
                Style::default()
                    .fg(self.theme.attention)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if let Some(branch) = branch {
            spans.push(Span::styled(
                format!(" {branch}"),
                Style::default().fg(self.theme.dim),
            ));
        }
        Line::from(spans)
    }

    /// Host identity is a separate, dim line and is never inferred here.
    fn host_line(&self, s: &SessionRosterEntry, text_w: u16) -> Line<'static> {
        let label = truncate(
            &format!("on {}", s.host),
            usize::from(text_w).saturating_sub(2),
        );
        Line::from(Span::styled(
            format!("  {label}"),
            Style::default().fg(self.theme.dim),
        ))
    }

    /// Render one agent row (phux-foz.9): lifecycle glyph, window name,
    /// then `state - agent-name` colored by state. The state segment keeps
    /// first claim on width — it is the row's information — with a small
    /// floor reserved for the window name so it stays identifiable.
    ///
    /// The glyph carries the attention ladder ([`attention_rank`]), not just
    /// the state: an UNSEEN `done` agent gets the filled diamond and bold —
    /// it finished and nobody has read the result — while a `done` agent whose
    /// pane you already visited relaxes to the same hollow ring as `idle`. A
    /// `working` agent gets the half-filled ring: alive, but wanting nothing.
    fn agent_line(&self, e: &AgentEntry, text_w: u16) -> Line<'static> {
        // ONE badge vocabulary for the whole chrome: the same call feeds
        // a pane's own title (`render::chrome::dividers`), so a working
        // agent can never be a `◐` here and something else there.
        let badge = crate::render::chrome::agent_badge(&self.theme, e.state, e.attention, e.seen);
        let color = badge.color;
        let glyph = badge.glyph;
        let avail = usize::from(text_w).saturating_sub(ICON_COLUMNS);
        let state_text = format!("{} - {}", e.state.as_str(), e.name);
        // A cross-session row is labelled by its SESSION, not its window: the
        // row's job is to say where in the fleet to go, and a window name
        // out of its session's context ("edit") locates nothing.
        let locator = e.session.as_ref().unwrap_or(&e.window_name);
        // The destination earns at least half the row. Previously the agent
        // description could reduce a long session name to five characters.
        let win_budget = avail
            .saturating_sub(display_width(&state_text) + 1)
            .max(avail / 2);
        let win_label = truncate(locator, win_budget);
        let state_budget = avail
            .saturating_sub(display_width(&win_label))
            .saturating_sub(1);
        let state_label = truncate(
            if display_width(&state_text) <= state_budget {
                &state_text
            } else {
                &e.name
            },
            state_budget,
        );
        let mut glyph_style = Style::default().fg(color);
        if badge.emphatic {
            glyph_style = glyph_style.add_modifier(Modifier::BOLD);
        }
        Line::from(vec![
            Span::styled(format!("{glyph} "), glyph_style),
            Span::styled(win_label, Style::default().fg(self.theme.text)),
            Span::styled(format!(" {state_label}"), Style::default().fg(color)),
        ])
    }

    /// Render one roster line (phux-k0cw): a status dot, the session name,
    /// and a right-aligned state histogram (`!1 *2`).
    ///
    /// The dot takes the session's worst rung via
    /// [`SessionRosterEntry::top_rank`], riding the SAME theme slots the
    /// agent rows use — a roster row and the agent row it summarizes must
    /// never disagree about colour. A satellite session paints dim with a
    /// `?` count: its per-Terminal metadata is not subscribable from here
    /// (`docs/spec/L3.md` §5), and an unknowable session must not render as
    /// a calm one.
    fn roster_line(&self, s: &SessionRosterEntry, text_w: u16) -> Line<'static> {
        let (dot, color) = self.roster_badge(s);
        let counts = roster_histogram(s);
        let avail = usize::from(text_w).saturating_sub(2);
        // Identity keeps at least half the row even under a large histogram.
        let name_budget = avail
            .saturating_sub(display_width(&counts) + usize::from(!counts.is_empty()))
            .max(avail / 2);
        let name = truncate(&s.name, name_budget);
        let counts = truncate(&counts, avail.saturating_sub(display_width(&name) + 1));
        let pad = avail
            .saturating_sub(display_width(&name))
            .saturating_sub(display_width(&counts));
        let style = if s.active {
            Style::default()
                .fg(self.theme.selection_fg)
                .add_modifier(Modifier::BOLD)
        } else if s.selectable {
            Style::default().fg(self.theme.text)
        } else {
            Style::default().fg(self.theme.dim)
        };
        let mut spans = vec![
            Span::styled(format!("{dot} "), Style::default().fg(color)),
            Span::styled(name, style),
        ];
        if !counts.is_empty() {
            spans.push(Span::styled(
                format!("{}{counts}", " ".repeat(pad)),
                Style::default().fg(color),
            ));
        }
        Line::from(spans)
    }

    const fn roster_badge(&self, s: &SessionRosterEntry) -> (&'static str, ratatui::style::Color) {
        if s.satellite {
            ("○", self.theme.dim)
        } else {
            match s.top_rank() {
                4 => ("●", self.theme.agent_blocked),
                3 => ("◆", self.theme.agent_done),
                2 => ("◐", self.theme.agent_working),
                1 => ("○", self.theme.agent_idle),
                _ => ("○", self.theme.dim),
            }
        }
    }

    /// Render the Agents `+N more` overflow row: dim and indented
    /// like an empty state, because it is chrome rather than a target you
    /// aim at — though clicking it does open the fleet dashboard.
    fn overflow_line(&self, hidden: usize, text_w: u16) -> Line<'static> {
        let label = truncate(
            &format!("+{hidden} {OVERFLOW_LABEL}"),
            usize::from(text_w).saturating_sub(2),
        );
        Line::from(Span::styled(
            format!("  {label}"),
            Style::default().fg(self.theme.dim),
        ))
    }

    /// Render an affordance row (phux-fce4), muted like the rest of the
    /// footer chrome. phux-foz.13: the leading action glyph (`+` / `=`)
    /// rides the slightly-brighter `sidebar_section` register — the same
    /// muted anchor color the section headers use — so the affordances read
    /// as deliberate, tappable chrome rather than an afterthought, while the
    /// word stays in the recessive `dim` tone.
    fn affordance_line(&self, label: &str, text_w: u16) -> Line<'static> {
        let label = truncate(label, usize::from(text_w));
        Line::from(self.affordance_spans(&label))
    }

    fn affordance_spans(&self, label: &str) -> Vec<Span<'static>> {
        let mut chars = label.chars();
        let glyph = chars.next().map(String::from).unwrap_or_default();
        let rest = chars.as_str().to_owned();
        vec![
            Span::styled(glyph, Style::default().fg(self.theme.chord)),
            Span::styled(rest, Style::default().fg(self.theme.text)),
        ]
    }

    /// Keep both global destinations permanently visible without taking a
    /// third row away from agents and sessions. The minimum sidebar width fits
    /// both labels; smaller embedded surfaces degrade to Commands alone.
    fn footer_actions_line(&self, text_w: u16) -> Line<'static> {
        let required = display_width(MENU_LABEL)
            + display_width(FOOTER_ACTION_GAP)
            + display_width(SETTINGS_LABEL);
        if required > usize::from(text_w) {
            return self.affordance_line(MENU_LABEL, text_w);
        }

        let mut spans = self.affordance_spans(MENU_LABEL);
        spans.push(Span::raw(FOOTER_ACTION_GAP));
        spans.extend(self.affordance_spans(SETTINGS_LABEL));
        Line::from(spans)
    }

    /// Render the sections + affordances + separator into a fresh
    /// `rect`-sized buffer, row-for-row from [`row_model`].
    fn compose(&self, rect: Rect) -> Buffer {
        let area = RataRect::new(0, 0, rect.w, rect.h);
        let mut buf = Buffer::empty(area);
        buf.set_style(
            area,
            Style::default().fg(self.theme.text).bg(self.theme.surface),
        );
        // One-cell gutters protect names from both the edge and separator.
        let text_w = rect.w.saturating_sub(1 + GUTTER * 2);
        let counts = self.counts();
        let model = row_model(counts, rect.h);
        let hidden = hidden_counts(counts, &model);
        if text_w > 0 {
            let lines: Vec<Line<'static>> = model
                .iter()
                .map(|row| self.row_line(*row, hidden, text_w))
                .collect();
            Paragraph::new(lines).render(RataRect::new(GUTTER, 0, text_w, rect.h), &mut buf);
            for (y, row) in (0..rect.h).zip(&model) {
                if self.row_selected(*row) {
                    buf.set_style(
                        RataRect::new(0, y, rect.w.saturating_sub(1), 1),
                        Style::default().bg(self.theme.selection_bg),
                    );
                }
            }
        }
        self.paint_separator(&mut buf, rect);
        buf
    }

    fn row_selected(&self, row: SidebarRow) -> bool {
        match row {
            SidebarRow::WindowName(i) => self.windows.get(i).is_some_and(|w| w.active),
            SidebarRow::RosterEntry(j) | SidebarRow::RosterHost(j) => {
                self.roster.get(j).is_some_and(|s| s.active)
            }
            _ => false,
        }
    }

    fn row_line(&self, row: SidebarRow, hidden: SidebarCounts, text_w: u16) -> Line<'static> {
        match row {
            SidebarRow::NeedsYouHeader => self.header_line(NEEDS_YOU_HEADER, text_w),
            SidebarRow::SpacesHeader => self.header_line(SPACES_HEADER, text_w),
            SidebarRow::AgentsEmpty => self.empty_line(AGENTS_EMPTY, text_w),
            SidebarRow::SessionsEmpty => self.empty_line(SESSIONS_EMPTY, text_w),
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
            SidebarRow::RosterHost(j) => self
                .roster
                .get(j)
                .map_or_else(|| Line::from(""), |s| self.host_line(s, text_w)),
            SidebarRow::NeedsYouOverflow => self.overflow_line(hidden.needs_you, text_w),
            SidebarRow::RosterOverflow => self.sessions_overflow_line(hidden, text_w),
            SidebarRow::Blank => Line::from(""),
            SidebarRow::NewWindow => self.affordance_line(NEW_LABEL, text_w),
            SidebarRow::Menu => self.footer_actions_line(text_w),
        }
    }

    fn sessions_overflow_line(&self, hidden: SidebarCounts, text_w: u16) -> Line<'static> {
        let mut parts = Vec::new();
        if hidden.roster > 0 {
            parts.push(format!("+{} sessions", hidden.roster));
        }
        if hidden.windows > 0 {
            parts.push(format!("+{} windows", hidden.windows));
        }
        if parts.is_empty() {
            parts.push(SPACES_HEADER.to_owned());
        }
        self.affordance_line(&parts.join(", "), text_w)
    }

    fn paint_separator(&self, buf: &mut Buffer, rect: Rect) {
        let sep_x = rect.w.saturating_sub(1);
        for y in 0..rect.h {
            if let Some(cell) = buf.cell_mut((sep_x, y)) {
                cell.set_symbol("│");
                cell.set_style(Style::default().fg(self.theme.border));
            }
        }
        // phux-foz.9: the collapse chevron claims the bottom corner cell
        // whenever the footer renders (same condition as `hit_test`).
        if collapse_visible(rect)
            && let Some(cell) = buf.cell_mut((sep_x, rect.h - 1))
        {
            cell.set_symbol(COLLAPSE_GLYPH);
            cell.set_style(Style::default().fg(self.theme.dim));
        }
    }
}

fn roster_histogram(s: &SessionRosterEntry) -> String {
    if s.satellite {
        return format!("?{}", s.total());
    }
    [("!", s.blocked), ("◆", s.done_unvisited), ("*", s.working)]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(glyph, n)| format!("{glyph}{n}"))
        .collect::<Vec<_>>()
        .join(" ")
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

/// Truncate `s` to `max` cells, marking the cut with `…`.
///
/// Delegates to the crate-wide [`clip_text`] so the sidebar, the pickers,
/// and the status bar all shorten text by the same rule — a divergence
/// here shows up as chrome that cuts three different ways on one screen.
fn truncate(s: &str, max: usize) -> String {
    clip_text(s, max)
}

/// Emit `buf` to `out` at `rect`'s origin, row by row, with a per-cell SGR
/// delta (shared with the overlay + status-bar painters).
///
/// A row is written as one uninterrupted run from its own `CUP`, so the
/// emitted cells have to advance the terminal's cursor exactly as many
/// columns as the strip reserved. A DOUBLE-WIDTH character advances two,
/// and ratatui leaves the cell it spills into empty; writing a space
/// there — as this did before phux-l96p.8's fix pass — advanced the row
/// one column too far per wide character, and a CJK window name walked
/// the whole strip out of its reserved columns and over the panes
/// beside it (ADR-0020). Skipping the spilled-into cells keeps the
/// column budget and the cursor in agreement.
///
/// Repaint complete changed rows so shortening a label clears its old tail.
/// Whole-row runs also preserve wide-glyph ownership across style changes.
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

    fn win(name: &str, active: bool) -> WindowInfo {
        WindowInfo {
            name: name.to_owned(),
            active,
            zoomed: false,
            attention: false,
            branch: None,
        }
    }

    fn win_attention(name: &str, active: bool) -> WindowInfo {
        WindowInfo {
            attention: true,
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
            window,
            window_name: window_name.to_owned(),
            pane: None,
            name: name.to_owned(),
            state,
            attention: false,
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
                    host: None
                }),
                Some(SessionRosterTarget {
                    name: "development".to_owned(),
                    host: Some("satellite-dev".to_owned())
                }),
                None,
            ]
        );
        let rect = Rect {
            x: 2,
            y: 3,
            w: 32,
            h: 18,
        };
        assert_eq!(
            hit_test(rect, targets.counts, 4, 12),
            Some(SidebarHit::Roster(0))
        );
        assert_eq!(
            hit_test(rect, targets.counts, 4, 13),
            Some(SidebarHit::Roster(0))
        );
        assert_eq!(
            hit_test(rect, targets.counts, 4, 14),
            Some(SidebarHit::Roster(1))
        );
        assert_eq!(
            hit_test(rect, targets.counts, 4, 15),
            Some(SidebarHit::Roster(1))
        );
    }

    #[test]
    fn changing_agent_state_repaints_only_that_stable_row() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_roster(vec![active_roster()]);
        let first = agent(0, "editor", "claude", AgentMetaState::Idle);
        let second = agent(1, "runner", "codex", AgentMetaState::Working);
        p.set_needs_you(vec![first.clone(), second.clone()]);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 36,
            h: 18,
        };
        paint_to_string(&mut p, rect);
        p.set_needs_you(vec![
            first,
            AgentEntry {
                state: AgentMetaState::Blocked,
                ..second
            },
        ]);
        let changed = paint_to_string(&mut p, rect);
        assert_eq!(rows_of(&changed).len(), 1);
        assert!(changed.starts_with("\x1b[3;1H"));
        assert!(strip_ansi(&changed).contains("codex"));
        assert!(paint_to_string(&mut p, rect).is_empty());
    }

    #[test]
    fn overflow_counts_sessions_and_windows_without_counting_host_lines() {
        let c = SidebarCounts {
            active_session: Some(0),
            ..counts(9, 5, 4)
        };
        let model = row_model(c, 14);
        let hidden = hidden_counts(c, &model);
        assert_eq!(hidden.needs_you, 5);
        assert_eq!(hidden.roster, 3);
        assert_eq!(hidden.windows, 3);
        assert!(model.contains(&SidebarRow::RosterOverflow));
        assert!(model.contains(&SidebarRow::RosterHost(0)));
        let p = SidebarPainter::new(Theme::default());
        let line = p.sessions_overflow_line(hidden, 32);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "+3 sessions, +3 windows");
    }

    #[test]
    fn tiny_viewports_and_huge_counts_remain_bounded() {
        let c = SidebarCounts {
            needs_you: usize::MAX,
            windows: usize::MAX,
            roster: usize::MAX,
            active_session: Some(0),
        };
        assert_eq!(row_model(c, 1), vec![SidebarRow::RosterOverflow]);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 1,
        };
        assert_eq!(hit_test(rect, c, 2, 0), Some(SidebarHit::Sessions));
        for h in 0..30 {
            let model = row_model(c, h);
            assert_eq!(model.len(), usize::from(h));
            assert_model_items(c, &model);
        }
        let invalid = SidebarCounts {
            active_session: Some(9),
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
        p.set_needs_you(vec![
            agent(0, "编辑器", "claude", AgentMetaState::Working);
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
        let buf = p.compose_buffer(rect);
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

    /// `session` and `pane` are display/commit inputs, so they MUST join the
    /// painter's content-cache key: two rows differing only by session are
    /// different rows, and a cache that conflated them would paint one
    /// session's queue while clicking through to another's.
    #[test]
    fn session_identity_participates_in_the_cache_key() {
        let local = agent(0, "edit", "claude", AgentMetaState::Working);
        let peer = AgentEntry {
            session: Some("phux-feat-auth".to_owned()),
            ..local.clone()
        };
        assert_ne!(local, peer, "session is part of row identity");

        let other_pane = AgentEntry {
            pane: Some(2),
            ..local.clone()
        };
        assert_ne!(local, other_pane, "pane ordinal is part of row identity");
    }

    /// The unreviewed-`done` row must be visually distinct from both a
    /// `working` row and a reviewed-`done` row — the glyph is what the user
    /// scans for.
    #[test]
    fn unreviewed_done_gets_its_own_glyph() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win("a", true)]);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 30,
            h: 12,
        };

        // phux-k0cw: the queue is zone 1, so its only row is index 1.
        let mut unseen = agent(0, "a", "claude", AgentMetaState::Done);
        unseen.seen = false;
        p.set_needs_you(vec![unseen.clone()]);
        let row = row_text(&p.compose_buffer(rect), rect, 1);
        assert!(row.contains('◆'), "unreviewed done: {row:?}");

        let seen = AgentEntry {
            seen: true,
            ..unseen
        };
        p.set_needs_you(vec![seen]);
        let row = row_text(&p.compose_buffer(rect), rect, 1);
        assert!(row.contains('○'), "reviewed done relaxes: {row:?}");

        p.set_needs_you(vec![agent(0, "a", "claude", AgentMetaState::Working)]);
        let row = row_text(&p.compose_buffer(rect), rect, 1);
        assert!(row.contains('◐'), "working: {row:?}");
    }

    /// `seen` is a real display input, so a flip must bust the paint cache —
    /// otherwise visiting a finished pane would leave the "look at me" glyph
    /// on screen.
    #[test]
    fn seen_flip_busts_the_paint_cache() {
        let mut p = SidebarPainter::new(Theme::default());
        let done = agent(0, "a", "claude", AgentMetaState::Done);
        assert!(p.set_needs_you(vec![done.clone()]));
        assert!(!p.set_needs_you(vec![done.clone()]));
        assert!(p.set_needs_you(vec![AgentEntry { seen: true, ..done }]));
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
        let rect = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 12,
        };
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
        let raw = paint_to_string(
            &mut p,
            Rect {
                x: 0,
                y: 0,
                w: 20,
                h: 14,
            },
        );
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
    fn places_rows_at_the_rect_origin() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win("a", true)]);
        // Right-docked: rect origin at column 60.
        let s = paint_to_string(
            &mut p,
            Rect {
                x: 60,
                y: 0,
                w: 20,
                h: 4,
            },
        );
        // First row CUP targets the rect's column (61, 1-based).
        assert!(s.contains("\x1b[1;61H"), "origin CUP missing: {s:?}");
    }

    #[test]
    fn unchanged_repaint_is_a_no_op() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_roster(vec![active_roster()]);
        p.set_windows(vec![win("a", true)]);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 16,
            h: 12,
        };
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
        p.set_needs_you(vec![agent(0, "b", "claude", AgentMetaState::Idle)]);
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
        let rect = Rect {
            x: 7,
            y: 2,
            w: 36,
            h: 24,
        };
        p.set_windows(vec![win("editor", true)]);
        p.set_roster(vec![active_roster()]);
        let full = paint_to_string(&mut p, rect);
        p.set_roster(vec![SessionRosterEntry {
            host: "devbox".to_owned(),
            ..active_roster()
        }]);
        let changed = paint_to_string(&mut p, rect);
        assert_eq!(
            changed.matches('H').count(),
            1,
            "one CUP for the changed host row"
        );
        assert!(changed.starts_with("\x1b[16;8H"));
        assert!(strip_ansi(&changed).contains("on devbox"));
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
        let rect = Rect {
            x: 0,
            y: 0,
            w: 36,
            h: 12,
        };
        let b = p.compose_buffer(rect);
        for y in [6, 7, 8] {
            assert_eq!(b[(0, y)].bg, p.theme.selection_bg);
            assert_eq!(b[(34, y)].bg, p.theme.selection_bg);
            assert_eq!(b[(35, y)].bg, p.theme.surface);
        }
        assert_eq!(b[(3, 7)].fg, p.theme.dim);
        for name in ["构建工具", "cafe\u{301}", "build"] {
            let line = p.roster_line(&roster(name, 1, 2, 0), 30);
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(display_width(&text), 30);
            assert!(text.ends_with("!1 *2"));
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

    /// phux-foz.1: a window whose pane asked for a human answer (ADR-0035)
    /// carries a `!` marker on its sidebar tab; unmarked tabs stay plain.
    /// The marker change also busts the paint cache.
    #[test]
    fn attention_window_gets_a_marker() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_roster(vec![active_roster()]);
        p.set_windows(vec![win("editor", true), win("shell", false)]);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 14,
        };
        let plain = strip_ansi(&paint_to_string(&mut p, rect));
        assert!(!plain.contains('!'), "no attention, no marker: {plain:?}");
        // The asking window gets the marker; the cache re-emits.
        assert!(
            p.set_windows(vec![win("editor", true), win_attention("shell", false)]),
            "attention flip must report a change"
        );
        let plain = strip_ansi(&paint_to_string(&mut p, rect));
        assert!(
            plain.contains("shell !"),
            "asking window tab must carry the marker: {plain:?}"
        );
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
        // phux-foz.9: same contract for the agents section — a state
        // flip (idle -> working) must repaint.
        let idle = agent(0, "a", "claude", AgentMetaState::Idle);
        assert!(p.set_needs_you(vec![idle.clone()]));
        assert!(!p.set_needs_you(vec![idle]));
        assert!(p.set_needs_you(vec![agent(0, "a", "claude", AgentMetaState::Working)]));
    }

    #[test]
    fn long_label_is_truncated_with_ellipsis() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_roster(vec![active_roster()]);
        p.set_windows(vec![win("a-very-long-window-title-indeed", true)]);
        let s = paint_to_string(
            &mut p,
            Rect {
                x: 0,
                y: 0,
                w: 12,
                h: 12,
            },
        );
        assert!(s.contains('…'), "overflowing label should be elided: {s:?}");
    }

    /// Windows expand compactly beneath the current session and its host.
    #[test]
    fn windows_nest_under_active_session_without_extra_sections() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_roster(vec![active_roster(), roster("peer", 0, 0, 0)]);
        p.set_windows(vec![
            win_branch("phux", true, "wave2/herdr"),
            win("scratch", false),
        ]);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 18,
        };
        let buf = p.compose_buffer(rect);
        assert!(row_text(&buf, rect, 8).contains(SPACES_HEADER));
        assert!(row_text(&buf, rect, 9).contains("development"));
        assert!(row_text(&buf, rect, 10).contains("on mini"));
        assert!(row_text(&buf, rect, 11).starts_with("   ● phux"));
        assert!(row_text(&buf, rect, 12).starts_with("   ○ scratch"));
        assert!(row_text(&buf, rect, 13).contains("peer"));
        assert!(row_text(&buf, rect, 14).contains("on mini"));
        assert!(!strip_text(&p, rect).contains("wave2/herdr"));
    }

    /// Every lifecycle stays in the caller's supplied display order.
    #[test]
    fn agents_section_renders_state_and_name_rows() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win("phux", true), win("scratch", false)]);
        p.set_needs_you(vec![
            agent(0, "phux", "claude", AgentMetaState::Idle),
            agent(1, "scratch", "merge-queue-w5", AgentMetaState::Working),
        ]);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 36,
            h: 14,
        };
        let buf = p.compose_buffer(rect);
        assert!(
            row_text(&buf, rect, 0).contains(NEEDS_YOU_HEADER),
            "the queue tops the strip: {:?}",
            row_text(&buf, rect, 0)
        );
        let claude_row = row_text(&buf, rect, 1);
        assert!(
            claude_row.contains("phux") && claude_row.contains("idle - claude"),
            "queue row shows locator + state - name: {claude_row:?}"
        );
        let worker_row = row_text(&buf, rect, 2);
        assert!(
            worker_row.contains("scratch") && worker_row.contains("merge-queue-w5"),
            "second queue row: {worker_row:?}"
        );
        assert!(
            row_text(&buf, rect, 3).trim().is_empty(),
            "gap before zone 2"
        );
        assert!(
            row_text(&buf, rect, 6).contains(SPACES_HEADER),
            "Sessions remains at the fixed midpoint: {:?}",
            row_text(&buf, rect, 6)
        );
        p.set_needs_you(vec![
            agent(0, "phux", "claude", AgentMetaState::Done),
            agent(1, "scratch", "merge-queue-w5", AgentMetaState::Blocked),
        ]);
        let changed = p.compose_buffer(rect);
        assert!(row_text(&changed, rect, 1).contains("claude"));
        assert!(row_text(&changed, rect, 2).contains("merge-queue-w5"));
        assert_eq!(row_text(&buf, rect, 6), row_text(&changed, rect, 6));
    }

    /// phux-k0cw: a cross-session queue row is labelled by its SESSION, not
    /// by its window — "edit" locates nothing once the row can come from
    /// anywhere on the server.
    #[test]
    fn a_foreign_queue_row_is_labelled_by_its_session() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win("phux", true)]);
        p.set_needs_you(vec![AgentEntry {
            session: Some("phux-feat-auth".to_owned()),
            pane: Some(1),
            ..agent(0, "edit", "claude", AgentMetaState::Blocked)
        }]);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 36,
            h: 14,
        };
        let buf = p.compose_buffer(rect);
        let row = row_text(&buf, rect, 1);
        assert!(
            row.contains("phux-feat-auth"),
            "foreign row names its session: {row:?}"
        );
        assert!(
            !row.contains("edit"),
            "window name is not the locator: {row:?}"
        );
    }

    /// Empty and busy populations never move the fixed headers.
    #[test]
    fn a_quiet_fleet_keeps_both_fixed_areas() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win_branch("phux", true, "main")]);
        // No agents wanting anything.
        let rect = Rect {
            x: 0,
            y: 0,
            w: 24,
            h: 12,
        };
        let buf = p.compose_buffer(rect);
        assert!(
            row_text(&buf, rect, 0).contains(NEEDS_YOU_HEADER),
            "Agents tops a calm strip: {:?}",
            row_text(&buf, rect, 0)
        );
        assert!(row_text(&buf, rect, 1).contains(AGENTS_EMPTY));
        assert!(row_text(&buf, rect, 5).contains(SPACES_HEADER));
        assert!(row_text(&buf, rect, 6).contains(SESSIONS_EMPTY));
        let counts = p.counts();
        assert_eq!(counts.needs_you, 0);
        assert!(
            row_model(counts, rect.h)
                .iter()
                .any(|r| matches!(r, SidebarRow::NeedsYouHeader)),
            "Agents header remains allocated when empty"
        );
    }

    /// Empty placeholders are inert.
    #[test]
    fn empty_sessions_section_shows_a_placeholder() {
        let p = SidebarPainter::new(Theme::default());
        // No windows, no agents, no peers.
        let rect = Rect {
            x: 0,
            y: 0,
            w: 24,
            h: 12,
        };
        let buf = p.compose_buffer(rect);
        assert!(
            row_text(&buf, rect, 5).contains(SPACES_HEADER),
            "Sessions header stays at midpoint: {:?}",
            row_text(&buf, rect, 5)
        );
        assert!(
            row_text(&buf, rect, 6).contains(SESSIONS_EMPTY),
            "empty Sessions section shows a placeholder: {:?}",
            row_text(&buf, rect, 6)
        );
        assert_eq!(
            hit_test(rect, p.counts(), 3, 6),
            None,
            "placeholder is inert"
        );
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
        let rect = Rect {
            x: 0,
            y: 0,
            w: 26,
            h: 16,
        };
        let buf = p.compose_buffer(rect);
        let model = row_model(p.counts(), rect.h);
        let first = model
            .iter()
            .position(|r| matches!(r, SidebarRow::RosterEntry(0)))
            .expect("roster row 0 allocated");
        let busy = row_text(&buf, rect, u16::try_from(first).unwrap());
        assert!(busy.contains("feat-auth"), "session name: {busy:?}");
        assert!(
            busy.contains("!1") && busy.contains("*2"),
            "histogram carries how much, not just what: {busy:?}"
        );
        assert!(row_text(&buf, rect, u16::try_from(first + 1).unwrap()).contains("on mini"));
        let sat = row_text(&buf, rect, u16::try_from(first + 2).unwrap());
        assert!(sat.contains("prod-3"), "satellite name: {sat:?}");
        assert!(
            sat.contains("?4"),
            "a satellite reads as unknown, never as a calm zero: {sat:?}"
        );
        assert!(row_text(&buf, rect, u16::try_from(first + 3).unwrap()).contains("on devbox"));
    }

    fn strip_text(p: &SidebarPainter, rect: Rect) -> String {
        let buf = p.compose_buffer(rect);
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
        let rect = Rect {
            x: 0,
            y: 0,
            w: 26,
            h: 18,
        };
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
        p.set_needs_you(queue);
        p.set_roster(vec![active_roster(), roster("feat-auth", 1, 0, 0)]);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 26,
            h: 18,
        };
        insta::assert_snapshot!(strip_text(&p, rect));
    }

    /// Short strips preserve a real session's name and host before overflow.
    #[test]
    fn sectioned_layout_snapshot_short() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![
            win_branch("phux", true, "main"),
            win("scratch", false),
        ]);
        p.set_needs_you(vec![agent(0, "phux", "codex", AgentMetaState::Blocked)]);
        p.set_roster(vec![active_roster(), roster("feat-auth", 0, 2, 0)]);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 26,
            h: 10,
        };
        insta::assert_snapshot!(strip_text(&p, rect));
    }

    #[test]
    fn sectioned_layout_snapshot_empty() {
        let p = SidebarPainter::new(Theme::default());
        let rect = Rect {
            x: 0,
            y: 0,
            w: 26,
            h: 18,
        };
        insta::assert_snapshot!(strip_text(&p, rect));
    }

    /// phux-fce4: the footer affordances render on the strip's last two
    /// rows when the strip is tall enough, and drop out below the minimum.
    /// phux-foz.9: the collapse chevron claims the bottom corner cell.
    #[test]
    fn footer_affordances_render_on_the_last_two_rows() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win("shell", true)]);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 28,
            h: 8,
        };
        let buf = p.compose_buffer(rect);
        assert!(
            row_text(&buf, rect, 6).contains(NEW_LABEL),
            "row 6 should hold the new affordance: {:?}",
            row_text(&buf, rect, 6)
        );
        assert!(
            row_text(&buf, rect, 7).contains(MENU_LABEL),
            "row 7 should hold the menu affordance: {:?}",
            row_text(&buf, rect, 7)
        );
        assert!(
            row_text(&buf, rect, 7).contains(SETTINGS_LABEL),
            "row 7 should hold the settings affordance: {:?}",
            row_text(&buf, rect, 7)
        );
        // The bottom corner cell carries the collapse chevron instead of
        // the separator rule.
        assert_eq!(buf[(27, 7)].symbol(), COLLAPSE_GLYPH);
        assert_eq!(buf[(27, 6)].symbol(), "│");
        // A 3-row strip is below the footer minimum: no affordances, no
        // chevron.
        let short = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 3,
        };
        let plain = strip_ansi(&paint_to_string(&mut p, short));
        assert!(
            !plain.contains(NEW_LABEL) && !plain.contains(MENU_LABEL),
            "short strip must not render the footer: {plain:?}"
        );
        assert!(
            !plain.contains(COLLAPSE_GLYPH),
            "short strip must not render the chevron: {plain:?}"
        );
    }

    /// phux-i0e8.10.3: every click the help table advertises resolves
    /// through the real [`hit_test`] to the target its row describes, on
    /// a strip tall enough to render the footer. The affordance rows
    /// match on the shared label consts, so renaming `+ new` / `= menu`
    /// without updating the table (or vice versa) breaks here.
    #[test]
    fn help_table_matches_hit_targets() {
        let rect = Rect {
            x: 0,
            y: 0,
            w: 28,
            h: 14,
        };
        let quiet = SidebarCounts {
            active_session: Some(0),
            ..counts(0, 1, 1)
        };
        for binding in HELP_BINDINGS {
            match binding.chord {
                "click" => {
                    assert_eq!(
                        hit_test(rect, quiet, 3, 9),
                        Some(SidebarHit::Window(0)),
                        "a window row click selects that window"
                    );
                }
                NEW_LABEL => {
                    assert_eq!(hit_test(rect, quiet, 3, 12), Some(SidebarHit::NewWindow));
                }
                MENU_LABEL => {
                    assert_eq!(hit_test(rect, quiet, 3, 13), Some(SidebarHit::Menu));
                }
                SETTINGS_LABEL => {
                    assert_eq!(hit_test(rect, quiet, 14, 13), Some(SidebarHit::Settings));
                }
                COLLAPSE_GLYPH => {
                    assert_eq!(hit_test(rect, quiet, 27, 13), Some(SidebarHit::Collapse));
                }
                NEEDS_YOU_HEADER => {
                    let tall = Rect { h: 12, ..rect };
                    assert_eq!(
                        hit_test(tall, counts(2, 1, 0), 3, 0),
                        Some(SidebarHit::Fleet),
                        "the Agents header opens the fleet"
                    );
                }
                SPACES_HEADER => {
                    let tall = Rect { h: 12, ..rect };
                    assert_eq!(
                        hit_test(tall, counts(0, 1, 2), 3, 5),
                        Some(SidebarHit::Sessions),
                        "the Sessions header opens host and session management"
                    );
                }
                OVERFLOW_LABEL => {
                    let tall = Rect { h: 16, ..rect };
                    let c = counts(9, 1, 0);
                    let row = row_model(c, tall.h)
                        .iter()
                        .position(|r| matches!(r, SidebarRow::NeedsYouOverflow))
                        .expect("overflow allocated");
                    assert_eq!(
                        hit_test(tall, c, 3, u16::try_from(row).unwrap()),
                        Some(SidebarHit::Fleet),
                        "an overflow row click opens the fleet dashboard"
                    );
                }
                other => panic!(
                    "help table row `{other}` has no adjacency check — \
                     add one that drives hit_test"
                ),
            }
        }
    }

    // ---------- phux-fce4 / phux-foz.9: row model + hit-test ----------

    fn counts(needs_you: usize, windows: usize, roster: usize) -> SidebarCounts {
        SidebarCounts {
            needs_you,
            windows,
            roster,
            active_session: None,
        }
    }

    #[test]
    fn row_model_reserves_footer_and_truncates_blocks() {
        let c = SidebarCounts {
            active_session: Some(0),
            ..counts(0, 3, 1)
        };
        let rows = row_model(c, 9);
        assert_eq!(rows.len(), 9);
        assert_eq!(rows[0], SidebarRow::NeedsYouHeader);
        assert_eq!(rows[1], SidebarRow::AgentsEmpty);
        assert_eq!(rows[3], SidebarRow::SpacesHeader);
        assert_eq!(rows[4], SidebarRow::RosterEntry(0));
        assert_eq!(rows[5], SidebarRow::RosterHost(0));
        assert_eq!(rows[6], SidebarRow::RosterOverflow);
        assert_eq!(rows[7], SidebarRow::NewWindow);
        assert_eq!(rows[8], SidebarRow::Menu);
        // Keep hidden windows reachable even when a pair and overflow cannot fit.
        let rows = row_model(c, 7);
        assert_eq!(rows[3], SidebarRow::RosterOverflow);
        assert_eq!(rows[4], SidebarRow::Blank);
        assert_eq!(rows[5], SidebarRow::NewWindow);
        assert_eq!(rows[6], SidebarRow::Menu);
        // Below the minimum height there is no footer.
        let rows = row_model(c, 3);
        assert_eq!(
            rows,
            vec![
                SidebarRow::NeedsYouHeader,
                SidebarRow::SpacesHeader,
                SidebarRow::RosterOverflow,
            ]
        );
    }

    /// Header positions depend only on viewport height.
    #[test]
    fn row_model_places_two_fixed_areas() {
        // 2 queued + 1 window in 10 rows.
        let rows = row_model(counts(2, 1, 0), 10);
        assert_eq!(
            rows,
            vec![
                SidebarRow::NeedsYouHeader,
                SidebarRow::NeedsYou(0),
                SidebarRow::NeedsYou(1),
                SidebarRow::Blank,
                SidebarRow::SpacesHeader,
                SidebarRow::SessionsEmpty,
                SidebarRow::Blank,
                SidebarRow::Blank,
                SidebarRow::NewWindow,
                SidebarRow::Menu,
            ]
        );
        // Adding sessions preserves both header positions.
        let rows = row_model(counts(0, 1, 2), 12);
        assert_eq!(rows[0], SidebarRow::NeedsYouHeader);
        assert_eq!(rows[5], SidebarRow::SpacesHeader);
        assert_eq!(rows[6], SidebarRow::RosterEntry(0));
        assert_eq!(rows[7], SidebarRow::RosterHost(0));
        assert_eq!(rows[8], SidebarRow::RosterEntry(1));
        assert_eq!(rows[9], SidebarRow::RosterHost(1));
        let rows = row_model(counts(0, 1, 0), 12);
        assert_eq!(rows[5], SidebarRow::SpacesHeader);
        assert_eq!(rows[6], SidebarRow::SessionsEmpty);
    }

    /// Agents overflow at the area's capacity, not a population-based cap.
    #[test]
    fn agents_overflow_declares_what_it_dropped() {
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

    /// Busy Agents never move the Sessions header or consume its capacity.
    #[test]
    fn agents_never_starve_sessions() {
        for h in MIN_FOOTER_HEIGHT..24 {
            let rows = row_model(counts(20, 2, 3), h);
            let body = usize::from(h).saturating_sub(2);
            assert_eq!(rows[0], SidebarRow::NeedsYouHeader);
            assert_eq!(rows[body / 2], SidebarRow::SpacesHeader);
            let quiet = row_model(counts(0, 2, 3), h);
            assert_eq!(&rows[body / 2..], &quiet[body / 2..]);
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
                    assert_eq!(rows.get(y + 1), Some(&SidebarRow::RosterHost(*j)));
                    sessions.push(*j);
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
        let rect = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 14,
        };
        let c = SidebarCounts {
            active_session: Some(0),
            ..counts(0, 2, 1)
        };
        assert_eq!(hit_test(rect, c, 3, 0), Some(SidebarHit::Fleet));
        assert_eq!(hit_test(rect, c, 3, 9), Some(SidebarHit::Window(0)));
        assert_eq!(hit_test(rect, c, 3, 10), Some(SidebarHit::Window(1)));
        // Padding rows miss.
        assert_eq!(hit_test(rect, c, 3, 5), None);
        // Footer rows.
        assert_eq!(hit_test(rect, c, 3, 12), Some(SidebarHit::NewWindow));
        assert_eq!(hit_test(rect, c, 3, 13), Some(SidebarHit::Menu));
    }

    /// Agent and session rows retain their own destinations under overflow.
    #[test]
    fn hit_test_maps_the_new_zones() {
        let rect = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 10,
        };
        // 2 queued + 1 window: rows 0 header, 1-2 queue, 3 gap, 4 `here`.
        let c = counts(2, 1, 0);
        assert_eq!(hit_test(rect, c, 3, 0), Some(SidebarHit::Fleet));
        assert_eq!(hit_test(rect, c, 3, 1), Some(SidebarHit::NeedsYou(0)));
        assert_eq!(hit_test(rect, c, 3, 2), Some(SidebarHit::NeedsYou(1)));
        assert_eq!(hit_test(rect, c, 3, 4), Some(SidebarHit::Sessions));
        assert_eq!(hit_test(rect, c, 3, 5), None, "empty Sessions placeholder");

        // Roster rows.
        let tall = Rect { h: 12, ..rect };
        let c = counts(0, 1, 2);
        assert_eq!(hit_test(tall, c, 3, 5), Some(SidebarHit::Sessions));
        assert_eq!(hit_test(tall, c, 3, 6), Some(SidebarHit::Roster(0)));
        assert_eq!(hit_test(tall, c, 3, 7), Some(SidebarHit::Roster(0)));
        assert_eq!(hit_test(tall, c, 3, 8), Some(SidebarHit::Roster(1)));
        assert_eq!(hit_test(tall, c, 3, 9), Some(SidebarHit::Roster(1)));

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

    /// phux-foz.9: the bottom corner cell is the collapse chevron — the
    /// only interactive cell on the separator column.
    #[test]
    fn hit_test_resolves_the_collapse_corner() {
        let rect = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 8,
        };
        let c = counts(0, 1, 0);
        assert_eq!(hit_test(rect, c, 19, 7), Some(SidebarHit::Collapse));
        // The rest of the separator column stays inert.
        assert_eq!(hit_test(rect, c, 19, 6), None);
        assert_eq!(hit_test(rect, c, 19, 0), None);
        // No footer (short strip) => no chevron target.
        let short = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 3,
        };
        assert_eq!(hit_test(short, c, 19, 2), None);
    }

    #[test]
    fn hit_test_respects_the_rect_origin_and_separator() {
        // Right-docked strip at x=60, y=2; agent 0 is at local row 1.
        let rect = Rect {
            x: 60,
            y: 2,
            w: 20,
            h: 8,
        };
        let c = counts(1, 0, 0);
        assert_eq!(hit_test(rect, c, 60, 3), Some(SidebarHit::NeedsYou(0)));
        // The separator column (last column of the strip) is not a target
        // outside the chevron corner.
        assert_eq!(hit_test(rect, c, 79, 0), None);
        // Outside the strip entirely.
        assert_eq!(hit_test(rect, c, 59, 1), None);
        assert_eq!(hit_test(rect, c, 80, 1), None);
        assert_eq!(hit_test(rect, c, 60, 10), None);
        // Degenerate rects never hit.
        assert_eq!(
            hit_test(
                Rect {
                    x: 0,
                    y: 0,
                    w: 0,
                    h: 0
                },
                c,
                0,
                0
            ),
            None
        );
    }

    /// Paint and hit-test derive from one row model: every row the painter
    /// fills with a window label hit-tests to that window, agent rows
    /// hit-test to their windows, and the footer rows hit-test to their
    /// affordances.
    #[test]
    fn paint_and_hit_test_agree_row_for_row() {
        let rect = Rect {
            x: 0,
            y: 0,
            w: 26,
            h: 14,
        };
        let windows = vec![
            win_branch("alpha", true, "main"),
            win("beta", false),
            win_branch("gamma", false, "dev"),
        ];
        let agents = vec![
            agent(1, "beta", "claude", AgentMetaState::Working),
            agent(2, "gamma", "codex", AgentMetaState::Idle),
        ];
        let peers = vec![roster("delta", 1, 0, 0), roster("epsilon", 0, 1, 0)];
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(windows.clone());
        p.set_needs_you(agents.clone());
        p.set_roster(peers.clone());
        let buf = p.compose_buffer(rect);
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
                    assert!(row_text(&buf, rect, y16).contains(&format!("on {}", peers[*j].host)));
                    assert_eq!(hit, Some(SidebarHit::Roster(*j)));
                }
                SidebarRow::NeedsYouOverflow => {
                    assert!(row_text(&buf, rect, y16).contains(OVERFLOW_LABEL));
                    assert_eq!(hit, Some(SidebarHit::Fleet));
                }
                SidebarRow::RosterOverflow => {
                    assert_eq!(hit, Some(SidebarHit::Sessions));
                }
                SidebarRow::Blank | SidebarRow::AgentsEmpty | SidebarRow::SessionsEmpty => {
                    assert_eq!(hit, None);
                }
                SidebarRow::NewWindow => {
                    assert!(row_text(&buf, rect, y16).contains(NEW_LABEL));
                    assert_eq!(hit, Some(SidebarHit::NewWindow));
                }
                SidebarRow::Menu => {
                    assert!(row_text(&buf, rect, y16).contains(MENU_LABEL));
                    assert!(row_text(&buf, rect, y16).contains(SETTINGS_LABEL));
                    assert_eq!(hit, Some(SidebarHit::Menu));
                    assert_eq!(hit_test(rect, c, 14, y16), Some(SidebarHit::Settings));
                }
            }
        }
    }
}
