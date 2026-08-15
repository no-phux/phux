//! Window sidebar painter (phux-4h5a, herdr-shaped by phux-p4vp/phux-fce4,
//! sectioned + agent-aware by phux-foz.9).
//!
//! A vertical strip laid out herdr-style in two labelled sections:
//!
//! - **`spaces`** — one two-row block per window: a status dot + the
//!   window's name (which upstream already resolves to the pane's live OSC
//!   title, phux-efj7, or its ADR-0040 agent label), with a dim branch line
//!   nested underneath when the window's focused pane sits inside a git
//!   repository (phux-p4vp).
//! - **`agents`** — one row per agent-running pane: a lifecycle glyph, the
//!   window's stored name, and `state - agent-name` colored by the agent's
//!   declared (ADR-0040) or inferred state. The driver builds these entries
//!   ([`AgentEntry`]) preferring the structured `phux.agent/v1` record and
//!   falling back to the OSC-title identity heuristic for plain
//!   `claude`/`codex` CLI panes that never declare one. When no pane is
//!   running an agent the section still renders its header with a quiet
//!   `no agents` empty-state line (phux-foz.13) so the strip reads as two
//!   composed sections rather than a bare window list.
//!
//! The strip's last two rows are the `+ new` / `= menu` affordances
//! (phux-fce4), bottom-anchored, with a collapse chevron in the bottom
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
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::agent_meta::AgentMetaState;
use crate::layout::Rect;
use crate::render::Theme;
use crate::render::clip_text;
use crate::render::overlay::HardcodedBinding;

/// Label of the "create" affordance row (phux-fce4).
///
/// Clicking it runs the `new-window` action — the sidebar lists windows,
/// so `+ new` creates one.
pub const NEW_LABEL: &str = "+ new";
/// Label of the "menu" affordance row (phux-fce4).
///
/// Clicking it opens the command palette — the one menu that covers
/// window, session, and plugin actions (`new-session` included) through
/// the action registry.
pub const MENU_LABEL: &str = "= menu";
/// Zone 1's header (phux-k0cw): the cross-session attention queue.
///
/// Phrased as a demand rather than a category (`agents`, the old label) on
/// purpose — the section answers "which of my agents needs me?", and it is
/// absent entirely when the answer is "none".
pub const NEEDS_YOU_HEADER: &str = "needs you";
/// Zone 2's header (phux-k0cw): the focused session's own windows.
pub const HERE_HEADER: &str = "here";
/// Zone 3's header (phux-k0cw) — herdr's word, now applied at herdr's
/// level: one line per OTHER session, not per window.
pub const SPACES_HEADER: &str = "spaces";
/// Empty-state placeholder for zone 2 (phux-foz.13, retargeted by
/// phux-k0cw) — shown in place of window blocks when the focused session
/// somehow has none, so the section reads as composed rather than vanishing.
pub const HERE_EMPTY: &str = "no windows";
/// Label of a truncated zone's overflow row (phux-k0cw). Clicking it opens
/// the agent-fleet dashboard, which is the surface that shows everything.
pub const OVERFLOW_LABEL: &str = "more";
/// How many rows zone 1 may claim before it overflows (phux-k0cw).
///
/// The queue is capped and the roster is not, and the asymmetry is the whole
/// design: the queue competes for the eye, so it must stay glanceable, while
/// the roster is meant to be COMPLETE — it answers "which sessions are on
/// the line?", a question a truncated list answers wrongly.
pub const NEEDS_YOU_CAP: usize = 5;
/// Rows zone 2 is guaranteed before zone 1 may claim any (phux-k0cw):
/// a header plus one two-row window block.
///
/// Without this floor a blocked fleet would squeeze the session you are
/// actually working in off its own strip.
pub const HERE_FLOOR: usize = 3;
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
        action: "jump to the agent that wants you (sidebar click)",
    },
    HardcodedBinding {
        chord: SPACES_HEADER,
        action: "switch to that session (sidebar roster click)",
    },
    HardcodedBinding {
        chord: OVERFLOW_LABEL,
        action: "open the agent-fleet dashboard (sidebar overflow click)",
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
        chord: COLLAPSE_GLYPH,
        action: "collapse the sidebar (bottom-corner click)",
    },
];

/// Minimum strip height (rows) at which the footer affordances render.
/// Below this every row goes to the section body — a 2–3 row strip
/// showing only chrome and no windows would be useless.
const MIN_FOOTER_HEIGHT: u16 = 4;

/// One agent-running pane, as the sidebar's `agents` section renders it
/// (phux-foz.9).
///
/// Built by the driver from the ADR-0040 `phux.agent/v1` record when the
/// pane declares one, else from the OSC-title identity heuristic
/// ([`crate::agent_meta::agent_name_from_title`]).
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
    /// pane as well as the window, so a queue row lands the user on the pane
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
    /// keeps `last_change` in a side map and lets it influence only the row
    /// ORDER.
    pub seen: bool,
}

/// Where an agent row sits on the attention ladder — higher demands a human
/// sooner.
///
/// The sidebar sorts its agent rows by this (descending), then by most recent
/// state change, so the row that needs a person is always on top.
///
/// ```text
/// blocked  >  done AND !seen  >  working  >  done/idle AND seen  >  unknown
/// ```
///
/// The load-bearing rung is the second: **"finished, and you have not looked
/// at it yet" outranks "still working"**. That is the entire "which of my nine
/// agents needs me?" feature — a `done` agent is holding a completed result
/// hostage until a human reads it, while a `working` agent needs nothing. Once
/// the user visits the pane (`seen`), the row drops to the quiet tier and stops
/// competing for the top of the strip.
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

/// One OTHER session, rolled up to a single roster line (phux-k0cw).
///
/// The roster is the answer to "which sessions are on the line?" — the
/// question the old session-local strip could not answer at all. It stays one
/// line per session on purpose: the queue above it is what competes for the
/// eye and is therefore capped, while the roster is meant to be COMPLETE.
/// Twelve sessions are twelve lines, not sixty.
///
/// The counts are carried rather than reduced to a single worst-state colour
/// because a dot says *what* and a count says *how much*: `!1 *2` is a
/// different morning than `!1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRosterEntry {
    /// The session's name — also what a click commits as
    /// `switch-session { name }`.
    pub name: String,
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
    /// Deliberately returns the SAME rungs [`attention_rank`] does, so zone
    /// 3's dot and zone 1's queue can never disagree about which session is
    /// the worst one. A roster row painted calm while one of its agents sits
    /// at the top of the queue would be the one bug that discredits the whole
    /// strip.
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
/// window/queue/roster lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SidebarCounts {
    /// Zone 1: panes wanting a human, across every session.
    pub needs_you: usize,
    /// Zone 2: windows in the focused session.
    pub windows: usize,
    /// Zone 3: other sessions on this server.
    pub roster: usize,
}

/// One row of the strip, top to bottom. Both the painter and [`hit_test`]
/// derive from this single model, so paint and click targets cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarRow {
    /// Zone 1's muted `needs you` header. Present only when the queue has
    /// at least one row — an empty queue contributes nothing at all.
    NeedsYouHeader,
    /// Queue entry `j`'s row (glyph + session/window + `state - name`).
    NeedsYou(usize),
    /// Zone 1's `+N more` row, when the queue is longer than
    /// [`NEEDS_YOU_CAP`]. Clicking it opens the fleet dashboard.
    NeedsYouOverflow,
    /// Zone 2's muted `here` header.
    HereHeader,
    /// Window `i`'s name row.
    WindowName(usize),
    /// Window `i`'s branch row (dim; blank when the window has no branch).
    WindowBranch(usize),
    /// Zone 2's empty-state placeholder: a quiet `no windows` line.
    HereEmpty,
    /// Zone 3's muted `spaces` header.
    SpacesHeader,
    /// Roster entry `j`'s row (dot + session name + state histogram).
    RosterEntry(usize),
    /// Zone 3's `+N more` row, when the roster does not fit the strip.
    RosterOverflow,
    /// Unused padding (section gap, or fill above the footer).
    Blank,
    /// The `+ new` affordance (create a window).
    NewWindow,
    /// The `= menu` affordance (open the command palette).
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
    /// A zone-2 window block — clicking selects window `i` (its
    /// `select-window` index). Both rows of a block hit.
    Window(usize),
    /// Zone 1's queue row `j`.
    NeedsYou(usize),
    /// Zone 3's roster row `j`.
    Roster(usize),
    /// Either zone's overflow row — clicking opens the agent-fleet
    /// dashboard, the surface that shows what the strip had to drop.
    Fleet,
    /// The `+ new` affordance.
    NewWindow,
    /// The `= menu` affordance.
    Menu,
    /// The collapse chevron in the bottom corner (phux-foz.9) —
    /// clicking runs `toggle-sidebar`.
    Collapse,
}

/// What a zone-1 queue row commits when clicked (phux-k0cw).
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
/// action. It is snapshotted per paint, which opens a staleness window: the
/// queue REORDERS as agents change state, so an index resolved against a
/// newer table could send the user somewhere they did not click. A
/// same-session `select-window` is forgiving of that; a `switch-session`
/// re-attach is not, which is why the dispatcher commits the resolved NAME
/// rather than re-deriving it.
#[derive(Debug, Clone, Default)]
pub struct SidebarTargets {
    /// The counts the frame was painted from — the same ones [`hit_test`]
    /// must be given, so a click resolves against the shape it landed on.
    pub counts: SidebarCounts,
    /// Zone 1's targets, in display order.
    pub needs_you: Vec<SidebarTarget>,
    /// Zone 3's session names, in display order.
    pub roster: Vec<String>,
}

/// The strip's row model for `counts` in an `h`-row rect (phux-k0cw).
///
/// Three zones, top to bottom:
///
/// 1. **`needs you`** — the cross-session attention queue, capped at
///    [`NEEDS_YOU_CAP`] rows plus one overflow row. It contributes
///    **exactly zero rows** when nothing wants a human: no header, no gap,
///    no placeholder. A sidebar that shrinks when you are calm is the point,
///    not an optimization — a strip that paints the same wall at rest as
///    under load has told you nothing by being present.
/// 2. **`here`** — the focused session's windows, one fixed two-row block
///    (name + branch) each. Guaranteed [`HERE_FLOOR`] rows before zone 1 may
///    claim any, so a blocked fleet cannot squeeze the session you are
///    working in off its own strip.
/// 3. **`spaces`** — one line per other session. Zero rows when there are
///    none, so a single-session user never sees an empty roster.
///
/// When `h >= MIN_FOOTER_HEIGHT` the bottom two rows are reserved for the
/// `+ new` / `= menu` affordances and body rows that would collide are
/// truncated. Fixed-size blocks keep the model derivable from the *counts*
/// alone, which is what lets the input dispatcher hit-test without
/// rebuilding the full projections.
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

    // Zone 1. Reserved FIRST (inverting the old window-greedy order) but
    // clamped so zone 2 keeps its floor: the queue outranks the local window
    // list for attention, never for existence.
    if counts.needs_you > 0 && body > 0 {
        let shown = counts.needs_you.min(NEEDS_YOU_CAP);
        let overflow = usize::from(counts.needs_you > shown);
        // header + rows + overflow, held back from zone 2's floor.
        let want = 1 + shown + overflow;
        let allowed = body.saturating_sub(HERE_FLOOR);
        let budget = want.min(allowed);
        // A header with no row under it is noise; skip the zone entirely.
        // At least one REAL row is guaranteed whenever the header renders —
        // a header over nothing but `+N more` tells the user less than the
        // single most-urgent row would.
        if budget >= 2 {
            rows.push(SidebarRow::NeedsYouHeader);
            let room = budget - 1;
            let listed = room.saturating_sub(overflow).max(1).min(shown).min(room);
            for j in 0..listed {
                rows.push(SidebarRow::NeedsYou(j));
            }
            if counts.needs_you > listed && rows.len() < budget {
                rows.push(SidebarRow::NeedsYouOverflow);
            }
        }
    }

    // Zone 2.
    if rows.len() < body {
        if !rows.is_empty() && rows.len() + 1 < body {
            rows.push(SidebarRow::Blank);
        }
        if rows.len() < body {
            rows.push(SidebarRow::HereHeader);
        }
    }
    if counts.windows == 0 && rows.len() < body {
        rows.push(SidebarRow::HereEmpty);
    }
    'blocks: for i in 0..counts.windows {
        for row in [SidebarRow::WindowName(i), SidebarRow::WindowBranch(i)] {
            if rows.len() >= body {
                break 'blocks;
            }
            // A truncated block may show a name row without its branch
            // row — a dangling name is still more useful than a blank.
            rows.push(row);
        }
    }

    // Zone 3, only when its gap + header + at least one row all fit.
    if counts.roster > 0 && rows.len() + 3 <= body {
        rows.push(SidebarRow::Blank);
        rows.push(SidebarRow::SpacesHeader);
        let mut listed = 0;
        for j in 0..counts.roster {
            if rows.len() >= body {
                break;
            }
            // Keep the last row for the overflow marker when more remain —
            // but never at the cost of listing no session at all, which
            // would leave a header over a bare `+N more`.
            let remaining = counts.roster - j;
            if remaining > 1 && listed > 0 && rows.len() + 1 >= body {
                break;
            }
            rows.push(SidebarRow::RosterEntry(j));
            listed += 1;
        }
        if counts.roster > listed && rows.len() < body {
            rows.push(SidebarRow::RosterOverflow);
        }
    }

    while rows.len() < body {
        rows.push(SidebarRow::Blank);
    }
    if footer == 2 {
        rows.push(SidebarRow::NewWindow);
        rows.push(SidebarRow::Menu);
    }
    rows
}

/// Resolve an outer-viewport mouse cell to a sidebar target.
///
/// `None` when it misses the strip (or lands on a header, the separator
/// column, or a blank row). `counts` must be the same shape the painter was
/// fed, so a click resolves against the frame it landed on. The bottom
/// corner cell — on the separator column, which is otherwise never a
/// target — is the collapse chevron (phux-foz.9).
#[must_use]
pub fn hit_test(rect: Rect, counts: SidebarCounts, x: u16, y: u16) -> Option<SidebarHit> {
    if rect.w == 0 || rect.h == 0 {
        return None;
    }
    // The bottom corner cell is the collapse chevron whenever the footer
    // renders (same condition the painter uses).
    if rect.h >= MIN_FOOTER_HEIGHT
        && rect.w >= 2
        && x == rect.x + rect.w - 1
        && y == rect.y + rect.h - 1
    {
        return Some(SidebarHit::Collapse);
    }
    // The rest of the last column is the separator rule, not a target.
    let text_w = rect.w.saturating_sub(1);
    if x < rect.x || x >= rect.x.saturating_add(text_w) {
        return None;
    }
    if y < rect.y || y >= rect.y.saturating_add(rect.h) {
        return None;
    }
    let row = usize::from(y - rect.y);
    match row_model(counts, rect.h).get(row)? {
        SidebarRow::WindowName(i) | SidebarRow::WindowBranch(i) => Some(SidebarHit::Window(*i)),
        SidebarRow::NeedsYou(j) => Some(SidebarHit::NeedsYou(*j)),
        SidebarRow::RosterEntry(j) => Some(SidebarHit::Roster(*j)),
        SidebarRow::NeedsYouOverflow | SidebarRow::RosterOverflow => Some(SidebarHit::Fleet),
        SidebarRow::NewWindow => Some(SidebarHit::NewWindow),
        SidebarRow::Menu => Some(SidebarHit::Menu),
        SidebarRow::NeedsYouHeader
        | SidebarRow::HereHeader
        | SidebarRow::SpacesHeader
        | SidebarRow::HereEmpty
        | SidebarRow::Blank => None,
    }
}

/// VT painter for the window sidebar.
#[derive(Debug)]
pub struct SidebarPainter {
    windows: Vec<WindowInfo>,
    needs_you: Vec<AgentEntry>,
    roster: Vec<SessionRosterEntry>,
    theme: Theme,
    /// Cache: the `(rect, windows, needs_you, roster)` of the last paint. An
    /// identical repaint is a zero-byte no-op.
    ///
    /// EVERY projection the strip paints must be in here. A zone left out of
    /// the key does not merely paint stale — it freezes for the life of the
    /// session, because the cache reports a hit forever.
    last: Option<(
        Rect,
        Vec<WindowInfo>,
        Vec<AgentEntry>,
        Vec<SessionRosterEntry>,
    )>,
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
        true
    }

    /// Replace zone 1's queue (phux-foz.9, cross-session per phux-k0cw).
    /// Same change-report contract as [`Self::set_windows`].
    pub fn set_needs_you(&mut self, needs_you: Vec<AgentEntry>) -> bool {
        if self.needs_you == needs_you {
            return false;
        }
        self.needs_you = needs_you;
        true
    }

    /// Replace zone 3's session roster (phux-k0cw). Same change-report
    /// contract as [`Self::set_windows`].
    pub fn set_roster(&mut self, roster: Vec<SessionRosterEntry>) -> bool {
        if self.roster == roster {
            return false;
        }
        self.roster = roster;
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
            roster: self.roster.iter().map(|s| s.name.clone()).collect(),
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
        if self.last.as_ref().is_some_and(|(r, w, n, s)| {
            *r == rect && *w == self.windows && *n == self.needs_you && *s == self.roster
        }) {
            return Ok(());
        }
        let buf = self.compose(rect);
        emit(out, &buf, rect)?;
        self.last = Some((
            rect,
            self.windows.clone(),
            self.needs_you.clone(),
            self.roster.clone(),
        ));
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

    /// The theme color for an agent lifecycle state (phux-foz.9).
    /// `Unknown` renders in the de-emphasis color — an undeclared state
    /// should not pretend to be information.
    const fn state_color(&self, state: AgentMetaState) -> Color {
        match state {
            AgentMetaState::Idle => self.theme.agent_idle,
            AgentMetaState::Working => self.theme.agent_working,
            AgentMetaState::Blocked => self.theme.agent_blocked,
            AgentMetaState::Done => self.theme.agent_done,
            AgentMetaState::Unknown => self.theme.dim,
        }
    }

    /// Render a muted lowercase section header (phux-foz.9).
    fn header_line(&self, label: &str, text_w: u16) -> Line<'static> {
        Line::from(Span::styled(
            truncate(label, usize::from(text_w)),
            Style::default().fg(self.theme.sidebar_section),
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
            .saturating_sub(2) // dot + space is 2 cells
            .saturating_sub(if w.attention { 2 } else { 0 });
        let label = truncate(&w.name, label_w);
        let style = if w.active {
            Style::default()
                .fg(self.theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(self.theme.action)
                .add_modifier(Modifier::BOLD)
        };
        let mut spans = vec![
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
        Line::from(spans)
    }

    /// Render one window's branch row (phux-p4vp): the focused pane's VCS
    /// branch, dim and nested under the label. Blank when unknown.
    fn branch_line(&self, w: &WindowInfo, text_w: u16) -> Line<'static> {
        let Some(branch) = w.branch.as_deref() else {
            return Line::from("");
        };
        let label = truncate(branch, usize::from(text_w).saturating_sub(2));
        Line::from(Span::styled(
            format!("  {label}"),
            Style::default()
                .fg(self.theme.dim)
                .add_modifier(Modifier::DIM),
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
        let color = self.state_color(e.state);
        let unreviewed_done = e.state == AgentMetaState::Done && !e.seen;
        let glyph = match e.state {
            AgentMetaState::Blocked => "●",
            // "look at me": finished, unread.
            AgentMetaState::Done if !e.seen => "◆",
            AgentMetaState::Working => "◐",
            AgentMetaState::Done | AgentMetaState::Idle | AgentMetaState::Unknown => "○",
        };
        let avail = usize::from(text_w).saturating_sub(2); // glyph + space
        let state_text = format!("{} - {}", e.state.as_str(), e.name);
        // A cross-session row is labelled by its SESSION, not its window: the
        // queue's job is to say where in the fleet to go, and a window name
        // out of its session's context ("edit") locates nothing.
        let locator = e.session.as_ref().unwrap_or(&e.window_name);
        let win_budget = avail
            .saturating_sub(state_text.chars().count() + 1)
            .max(avail.min(5));
        let win_label = truncate(locator, win_budget.min(locator.chars().count()));
        let state_budget = avail
            .saturating_sub(win_label.chars().count())
            .saturating_sub(1);
        let state_label = truncate(&state_text, state_budget);
        let mut glyph_style = Style::default().fg(color);
        if e.attention || unreviewed_done {
            glyph_style = glyph_style.add_modifier(Modifier::BOLD);
        }
        Line::from(vec![
            Span::styled(format!("{glyph} "), glyph_style),
            Span::styled(win_label, Style::default().fg(self.theme.action)),
            Span::styled(format!(" {state_label}"), Style::default().fg(color)),
        ])
    }

    /// Render one roster line (phux-k0cw): a status dot, the session name,
    /// and a right-aligned state histogram (`!1 *2`).
    ///
    /// The dot takes the session's worst rung via
    /// [`SessionRosterEntry::top_rank`], riding the SAME theme slots the
    /// queue rows use — a roster row and the queue row it summarizes must
    /// never disagree about colour. A satellite session paints dim with a
    /// `?` count: its per-Terminal metadata is not subscribable from here
    /// (`docs/spec/L3.md` §5), and an unknowable session must not render as
    /// a calm one.
    fn roster_line(&self, s: &SessionRosterEntry, text_w: u16) -> Line<'static> {
        let (dot, color) = if s.satellite {
            ("○", self.theme.dim)
        } else {
            match s.top_rank() {
                4 => ("●", self.theme.agent_blocked),
                3 => ("◆", self.theme.agent_done),
                2 => ("◐", self.theme.agent_working),
                1 => ("○", self.theme.agent_idle),
                _ => ("○", self.theme.dim),
            }
        };
        // The histogram is the "how much" the dot cannot carry. Only
        // non-zero rungs appear, worst first, so the common calm case adds
        // no noise at all.
        let mut counts = String::new();
        if s.satellite {
            counts = format!("?{}", s.total());
        } else {
            for (glyph, n) in [
                ("!", s.blocked),
                ("\u{25c6}", s.done_unvisited),
                ("*", s.working),
            ] {
                if n > 0 {
                    if !counts.is_empty() {
                        counts.push(' ');
                    }
                    counts.push_str(&format!("{glyph}{n}"));
                }
            }
        }
        let avail = usize::from(text_w).saturating_sub(2); // dot + space
        let name_budget = avail.saturating_sub(if counts.is_empty() {
            0
        } else {
            counts.chars().count() + 1
        });
        let name = truncate(&s.name, name_budget);
        let pad = avail
            .saturating_sub(name.chars().count())
            .saturating_sub(counts.chars().count());
        let mut spans = vec![
            Span::styled(format!("{dot} "), Style::default().fg(color)),
            Span::styled(name, Style::default().fg(self.theme.action)),
        ];
        if !counts.is_empty() {
            spans.push(Span::styled(
                format!("{}{counts}", " ".repeat(pad)),
                Style::default().fg(color),
            ));
        }
        Line::from(spans)
    }

    /// Render a zone's `+N more` overflow row (phux-k0cw): dim and indented
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
        let label = truncate(label, usize::from(text_w).saturating_sub(2));
        let mut chars = label.chars();
        let glyph = chars.next().map(String::from).unwrap_or_default();
        let rest = chars.as_str().to_owned();
        Line::from(vec![
            Span::styled("  ", Style::default().fg(self.theme.dim)),
            Span::styled(glyph, Style::default().fg(self.theme.sidebar_section)),
            Span::styled(rest, Style::default().fg(self.theme.dim)),
        ])
    }

    /// Render the sections + affordances + separator into a fresh
    /// `rect`-sized buffer, row-for-row from [`row_model`].
    fn compose(&self, rect: Rect) -> Buffer {
        let area = RataRect::new(0, 0, rect.w, rect.h);
        let mut buf = Buffer::empty(area);
        // Text occupies every column except the 1-cell right separator.
        let text_w = rect.w.saturating_sub(1);
        let counts = self.counts();
        let model = row_model(counts, rect.h);
        // What each zone had to drop, for its overflow row.
        let shown_queue = model
            .iter()
            .filter(|r| matches!(r, SidebarRow::NeedsYou(_)))
            .count();
        let shown_roster = model
            .iter()
            .filter(|r| matches!(r, SidebarRow::RosterEntry(_)))
            .count();
        if text_w > 0 {
            let lines: Vec<Line<'static>> = model
                .iter()
                .map(|row| match row {
                    SidebarRow::NeedsYouHeader => self.header_line(NEEDS_YOU_HEADER, text_w),
                    SidebarRow::HereHeader => self.header_line(HERE_HEADER, text_w),
                    SidebarRow::SpacesHeader => self.header_line(SPACES_HEADER, text_w),
                    SidebarRow::WindowName(i) => self
                        .windows
                        .get(*i)
                        .map_or_else(|| Line::from(""), |w| self.name_line(w, text_w)),
                    SidebarRow::WindowBranch(i) => self
                        .windows
                        .get(*i)
                        .map_or_else(|| Line::from(""), |w| self.branch_line(w, text_w)),
                    SidebarRow::NeedsYou(j) => self
                        .needs_you
                        .get(*j)
                        .map_or_else(|| Line::from(""), |e| self.agent_line(e, text_w)),
                    SidebarRow::RosterEntry(j) => self
                        .roster
                        .get(*j)
                        .map_or_else(|| Line::from(""), |s| self.roster_line(s, text_w)),
                    SidebarRow::NeedsYouOverflow => {
                        self.overflow_line(counts.needs_you.saturating_sub(shown_queue), text_w)
                    }
                    SidebarRow::RosterOverflow => {
                        self.overflow_line(counts.roster.saturating_sub(shown_roster), text_w)
                    }
                    SidebarRow::HereEmpty => self.empty_line(HERE_EMPTY, text_w),
                    SidebarRow::Blank => Line::from(""),
                    SidebarRow::NewWindow => self.affordance_line(NEW_LABEL, text_w),
                    SidebarRow::Menu => self.affordance_line(MENU_LABEL, text_w),
                })
                .collect();
            Paragraph::new(lines).render(RataRect::new(0, 0, text_w, rect.h), &mut buf);
        }
        // Vertical rule down the strip's last column.
        let sep_x = rect.w.saturating_sub(1);
        for y in 0..rect.h {
            if let Some(cell) = buf.cell_mut((sep_x, y)) {
                cell.set_symbol("│");
                cell.set_style(Style::default().fg(self.theme.border));
            }
        }
        // phux-foz.9: the collapse chevron claims the bottom corner cell
        // whenever the footer renders (same condition as `hit_test`).
        if rect.h >= MIN_FOOTER_HEIGHT
            && rect.w >= 2
            && let Some(cell) = buf.cell_mut((sep_x, rect.h - 1))
        {
            cell.set_symbol(COLLAPSE_GLYPH);
            cell.set_style(Style::default().fg(self.theme.dim));
        }
        buf
    }
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
fn emit<W: Write>(out: &mut W, buf: &Buffer, rect: Rect) -> io::Result<()> {
    for row in 0..rect.h {
        write!(out, "\x1b[{};{}H\x1b[0m", rect.y + row + 1, rect.x + 1)?;
        let mut prev_styled = false;
        for col in 0..rect.w {
            let cell = &buf[(col, row)];
            crate::render::sgr::emit_cell_sgr(out, cell, &mut prev_styled)?;
            let sym = cell.symbol();
            if sym.is_empty() {
                out.write_all(b" ")?;
            } else {
                out.write_all(sym.as_bytes())?;
            }
        }
        out.write_all(b"\x1b[0m")?;
    }
    out.flush()
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
            blocked,
            working,
            done_unvisited,
            settled: 0,
            unknown: 0,
            satellite: false,
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

    /// A roster row's dot and the queue's ordering must agree about severity.
    /// They are two renderings of ONE ladder, so `top_rank` returns the same
    /// rungs `attention_rank` does — a session painted calm while one of its
    /// agents sits at the top of the queue would discredit the whole strip.
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
        assert_eq!(local, local.clone(), "otherwise identical rows still match");
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
        p.set_windows(vec![win("editor", false), win("shell", true)]);
        let raw = paint_to_string(
            &mut p,
            Rect {
                x: 0,
                y: 0,
                w: 20,
                h: 10,
            },
        );
        let plain = strip_ansi(&raw);
        assert!(plain.contains("editor"), "first tab label: {plain:?}");
        assert!(plain.contains("shell"), "second tab label: {plain:?}");
        // phux-k0cw: with a quiet fleet the focused session's header tops
        // the strip — `spaces` now belongs to the peer roster below it.
        assert!(plain.contains(HERE_HEADER), "here header: {plain:?}");
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
        p.set_windows(vec![win("a", true)]);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 16,
            h: 6,
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
        // phux-foz.9: an agent change alone invalidates it too.
        paint_to_string(&mut p, rect);
        p.set_needs_you(vec![agent(0, "b", "claude", AgentMetaState::Idle)]);
        assert!(
            !paint_to_string(&mut p, rect).is_empty(),
            "changed agents must re-emit"
        );
    }

    /// phux-foz.1: a window whose pane asked for a human answer (ADR-0035)
    /// carries a `!` marker on its sidebar tab; unmarked tabs stay plain.
    /// The marker change also busts the paint cache.
    #[test]
    fn attention_window_gets_a_marker() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win("editor", true), win("shell", false)]);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 10,
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
        // phux-p4vp: a branch change alone busts the cache too — a
        // `git switch` must repaint the branch line.
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
        p.set_windows(vec![win("a-very-long-window-title-indeed", true)]);
        let s = paint_to_string(
            &mut p,
            Rect {
                x: 0,
                y: 0,
                w: 12,
                h: 3,
            },
        );
        assert!(s.contains('…'), "overflowing label should be elided: {s:?}");
    }

    /// phux-p4vp: a window with a branch renders it dim on the row under
    /// its label, herdr-style; a window without one leaves the row blank.
    #[test]
    fn branch_renders_on_the_row_under_the_label() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![
            win_branch("phux", true, "wave2/herdr"),
            win("scratch", false),
        ]);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 10,
        };
        let plain = strip_ansi(&paint_to_string(&mut p, rect));
        assert!(
            plain.contains("wave2/herdr"),
            "branch line missing: {plain:?}"
        );
        // Row order under the header: name, branch, next name — check via
        // the composed buffer, whose rows are addressable.
        let buf = p.compose_buffer(rect);
        assert!(
            row_text(&buf, rect, 0).contains(HERE_HEADER),
            "row 0 is the focused session's header: {:?}",
            row_text(&buf, rect, 0)
        );
        assert!(
            row_text(&buf, rect, 1).contains("phux"),
            "row 1: {:?}",
            row_text(&buf, rect, 1)
        );
        assert!(
            row_text(&buf, rect, 2).contains("wave2/herdr"),
            "row 2: {:?}",
            row_text(&buf, rect, 2)
        );
        assert!(
            row_text(&buf, rect, 3).contains("scratch"),
            "row 3: {:?}",
            row_text(&buf, rect, 3)
        );
        assert!(
            row_text(&buf, rect, 4).trim().is_empty(),
            "branchless window's branch row must be blank: {:?}",
            row_text(&buf, rect, 4)
        );
    }

    /// phux-foz.9: the agents section renders under the spaces blocks — a
    /// blank gap, the muted `agents` header, then one row per entry
    /// showing glyph + window name + `state - agent-name`.
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
        // phux-k0cw: the queue is now the TOP zone, not a section under the
        // window list — rows 0 header, 1-2 queue, 3 gap, 4 `here` header.
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
            worker_row.contains("working - merge-queue-w5"),
            "second queue row: {worker_row:?}"
        );
        assert!(
            row_text(&buf, rect, 3).trim().is_empty(),
            "gap before zone 2"
        );
        assert!(
            row_text(&buf, rect, 4).contains(HERE_HEADER),
            "zone 2 follows the queue: {:?}",
            row_text(&buf, rect, 4)
        );
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

    /// phux-k0cw, THE load-bearing property: when nothing wants a human the
    /// queue contributes exactly zero rows — no header, no gap, no
    /// placeholder. A strip that paints the same wall at rest as under load
    /// has told you nothing by being present, and this is the one behaviour
    /// the competitor's structural sidebar cannot have.
    #[test]
    fn a_quiet_fleet_gives_zone_one_no_rows_at_all() {
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
            row_text(&buf, rect, 0).contains(HERE_HEADER),
            "zone 2 tops a calm strip: {:?}",
            row_text(&buf, rect, 0)
        );
        for y in 0..rect.h {
            let row = row_text(&buf, rect, y);
            assert!(
                !row.contains(NEEDS_YOU_HEADER),
                "row {y} advertises an empty queue: {row:?}"
            );
        }
        let counts = p.counts();
        assert_eq!(counts.needs_you, 0);
        assert!(
            !row_model(counts, rect.h)
                .iter()
                .any(|r| matches!(r, SidebarRow::NeedsYouHeader)),
            "no header is allocated for an empty queue"
        );
    }

    /// phux-foz.13, retargeted by phux-k0cw: a focused session with no
    /// windows shows its own quiet placeholder rather than a bare header.
    /// The placeholder is inert (not a click target).
    #[test]
    fn empty_here_section_shows_a_placeholder() {
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
            row_text(&buf, rect, 0).contains(HERE_HEADER),
            "here header tops the strip: {:?}",
            row_text(&buf, rect, 0)
        );
        assert!(
            row_text(&buf, rect, 1).contains(HERE_EMPTY),
            "empty here section shows a placeholder: {:?}",
            row_text(&buf, rect, 1)
        );
        assert_eq!(
            hit_test(rect, p.counts(), 3, 1),
            None,
            "placeholder is inert"
        );
    }

    /// phux-k0cw: the roster rolls a session to one line — a dot plus a
    /// histogram — and a satellite session, whose per-Terminal metadata is
    /// not subscribable from here, is painted explicitly unknown rather than
    /// being allowed to read as calm.
    #[test]
    fn the_roster_renders_one_line_per_session_with_counts() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win("phux", true)]);
        p.set_roster(vec![
            roster("feat-auth", 1, 2, 0),
            SessionRosterEntry {
                unknown: 4,
                satellite: true,
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
        let sat = row_text(&buf, rect, u16::try_from(first + 1).unwrap());
        assert!(sat.contains("prod-3"), "satellite name: {sat:?}");
        assert!(
            sat.contains("?4"),
            "a satellite reads as unknown, never as a calm zero: {sat:?}"
        );
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

    /// phux-k0cw: the CALM shape. One snapshot cannot pin an allocator whose
    /// defining property is that a zone disappears, so the three states get
    /// three snapshots. This is the one a user sees most: no queue at all,
    /// the focused session on top, peers rolled to a line each.
    #[test]
    fn sectioned_layout_snapshot_quiet() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![
            win_branch("phux", true, "main"),
            win("scratch", false),
        ]);
        p.set_roster(vec![
            roster("feat-auth", 0, 1, 0),
            SessionRosterEntry {
                unknown: 2,
                satellite: true,
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

    /// phux-k0cw: the ATTENTION shape — the queue at its cap with an honest
    /// overflow row, pushing zone 2 down but never off.
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
        for i in 0..5 {
            queue.push(AgentEntry {
                session: Some(format!("wave-{i}")),
                pane: Some(0),
                ..agent(0, "run", "claude", AgentMetaState::Working)
            });
        }
        p.set_needs_you(queue);
        p.set_roster(vec![roster("feat-auth", 1, 0, 0)]);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 26,
            h: 18,
        };
        insta::assert_snapshot!(strip_text(&p, rect));
    }

    /// phux-k0cw: the SHORT-STRIP shape — zone 3 is the first to go, zone 2
    /// keeps its floor, and nothing dangles.
    #[test]
    fn sectioned_layout_snapshot_short() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![
            win_branch("phux", true, "main"),
            win("scratch", false),
        ]);
        p.set_needs_you(vec![agent(0, "phux", "codex", AgentMetaState::Blocked)]);
        p.set_roster(vec![roster("feat-auth", 0, 2, 0)]);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 26,
            h: 10,
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
            w: 20,
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
        // The bottom corner cell carries the collapse chevron instead of
        // the separator rule.
        assert_eq!(buf[(19, 7)].symbol(), COLLAPSE_GLYPH);
        assert_eq!(buf[(19, 6)].symbol(), "│");
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
        // 1 window, footer rendered: rows 0 header, 1-2 window block,
        // 6 `+ new`, 7 `= menu`, corner (19, 7) collapse.
        let rect = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 8,
        };
        let quiet = counts(0, 1, 0);
        for binding in HELP_BINDINGS {
            match binding.chord {
                "click" => {
                    assert_eq!(
                        hit_test(rect, quiet, 3, 1),
                        Some(SidebarHit::Window(0)),
                        "a window row click selects that window"
                    );
                }
                NEW_LABEL => {
                    assert_eq!(hit_test(rect, quiet, 3, 6), Some(SidebarHit::NewWindow));
                }
                MENU_LABEL => {
                    assert_eq!(hit_test(rect, quiet, 3, 7), Some(SidebarHit::Menu));
                }
                COLLAPSE_GLYPH => {
                    assert_eq!(hit_test(rect, quiet, 19, 7), Some(SidebarHit::Collapse));
                }
                NEEDS_YOU_HEADER => {
                    // A taller strip so the queue and zone 2 both fit.
                    let tall = Rect { h: 12, ..rect };
                    assert_eq!(
                        hit_test(tall, counts(2, 1, 0), 3, 1),
                        Some(SidebarHit::NeedsYou(0)),
                        "a queue row click jumps to that agent"
                    );
                }
                SPACES_HEADER => {
                    let tall = Rect { h: 12, ..rect };
                    assert_eq!(
                        hit_test(tall, counts(0, 1, 2), 3, 5),
                        Some(SidebarHit::Roster(0)),
                        "a roster row click switches session"
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
        }
    }

    #[test]
    fn row_model_reserves_footer_and_truncates_blocks() {
        // 3 windows, quiet fleet, 9 rows: `here` header + 6 window-area
        // rows fit 3 blocks, and zone 1 costs nothing.
        let rows = row_model(counts(0, 3, 0), 9);
        assert_eq!(rows.len(), 9);
        assert_eq!(rows[0], SidebarRow::HereHeader);
        assert_eq!(rows[1], SidebarRow::WindowName(0));
        assert_eq!(rows[2], SidebarRow::WindowBranch(0));
        assert_eq!(rows[5], SidebarRow::WindowName(2));
        assert_eq!(rows[6], SidebarRow::WindowBranch(2));
        assert_eq!(rows[7], SidebarRow::NewWindow);
        assert_eq!(rows[8], SidebarRow::Menu);
        // 3 windows in 7 rows: 5 body rows truncate the third block.
        let rows = row_model(counts(0, 3, 0), 7);
        assert_eq!(rows[4], SidebarRow::WindowBranch(1));
        assert_eq!(rows[5], SidebarRow::NewWindow);
        assert_eq!(rows[6], SidebarRow::Menu);
        // Below the minimum height there is no footer.
        let rows = row_model(counts(0, 1, 0), 3);
        assert_eq!(
            rows,
            vec![
                SidebarRow::HereHeader,
                SidebarRow::WindowName(0),
                SidebarRow::WindowBranch(0),
            ]
        );
    }

    /// phux-k0cw: the three zones in order, with the queue on top.
    #[test]
    fn row_model_places_the_three_zones() {
        // 2 queued + 1 window in 10 rows.
        let rows = row_model(counts(2, 1, 0), 10);
        assert_eq!(
            rows,
            vec![
                SidebarRow::NeedsYouHeader,
                SidebarRow::NeedsYou(0),
                SidebarRow::NeedsYou(1),
                SidebarRow::Blank,
                SidebarRow::HereHeader,
                SidebarRow::WindowName(0),
                SidebarRow::WindowBranch(0),
                SidebarRow::Blank,
                SidebarRow::NewWindow,
                SidebarRow::Menu,
            ]
        );
        // Add a roster: it lands last, behind its own gap + header.
        let rows = row_model(counts(0, 1, 2), 12);
        assert_eq!(rows[0], SidebarRow::HereHeader);
        assert_eq!(rows[4], SidebarRow::SpacesHeader);
        assert_eq!(rows[5], SidebarRow::RosterEntry(0));
        assert_eq!(rows[6], SidebarRow::RosterEntry(1));
        // No peers => no roster header at all, so a single-session user
        // never reads an empty section.
        let rows = row_model(counts(0, 1, 0), 12);
        assert!(!rows.contains(&SidebarRow::SpacesHeader), "{rows:?}");
    }

    /// phux-k0cw: the queue is capped and says so. Nine agents wanting a
    /// human do not get nine rows — they get [`NEEDS_YOU_CAP`] plus one
    /// honest `+N more`, because a queue that fills the strip is the wall
    /// this design exists to avoid.
    #[test]
    fn the_queue_caps_and_declares_what_it_dropped() {
        let rows = row_model(counts(9, 1, 0), 16);
        let listed = rows
            .iter()
            .filter(|r| matches!(r, SidebarRow::NeedsYou(_)))
            .count();
        assert_eq!(listed, NEEDS_YOU_CAP, "{rows:?}");
        assert!(rows.contains(&SidebarRow::NeedsYouOverflow), "{rows:?}");
        // Exactly at the cap there is nothing to declare.
        let rows = row_model(counts(NEEDS_YOU_CAP, 1, 0), 16);
        assert!(!rows.contains(&SidebarRow::NeedsYouOverflow), "{rows:?}");
    }

    /// phux-k0cw: zone 2 keeps its floor. A blocked fleet must not squeeze
    /// the session you are actually working in off its own strip.
    #[test]
    fn the_queue_never_starves_the_focused_session() {
        for h in MIN_FOOTER_HEIGHT..24 {
            let rows = row_model(counts(20, 2, 3), h);
            let body = usize::from(h).saturating_sub(2);
            let queue = rows
                .iter()
                .filter(|r| {
                    matches!(
                        r,
                        SidebarRow::NeedsYouHeader
                            | SidebarRow::NeedsYou(_)
                            | SidebarRow::NeedsYouOverflow
                    )
                })
                .count();
            assert!(
                queue == 0 || queue <= body.saturating_sub(HERE_FLOOR),
                "h={h}: queue took {queue} of {body} body rows: {rows:?}"
            );
            if body >= HERE_FLOOR {
                assert!(
                    rows.contains(&SidebarRow::HereHeader),
                    "h={h}: the focused session lost its header: {rows:?}"
                );
            }
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

                        if needs_you == 0 {
                            assert!(
                                !rows.iter().any(|r| matches!(
                                    r,
                                    SidebarRow::NeedsYouHeader
                                        | SidebarRow::NeedsYou(_)
                                        | SidebarRow::NeedsYouOverflow
                                )),
                                "a calm fleet costs zero rows: {c:?} h={h} {rows:?}"
                            );
                        }
                        if roster == 0 {
                            assert!(
                                !rows.iter().any(|r| matches!(
                                    r,
                                    SidebarRow::SpacesHeader | SidebarRow::RosterEntry(_)
                                )),
                                "no peers costs zero rows: {c:?} h={h}"
                            );
                        }

                        // No index is ever allocated twice — a repeat would
                        // paint one entry over another and mis-resolve its
                        // click.
                        let mut seen_q = Vec::new();
                        let mut seen_r = Vec::new();
                        let mut seen_w = Vec::new();
                        for row in &rows {
                            match row {
                                SidebarRow::NeedsYou(j) => seen_q.push(*j),
                                SidebarRow::RosterEntry(j) => seen_r.push(*j),
                                SidebarRow::WindowName(i) => seen_w.push(*i),
                                _ => {}
                            }
                        }
                        for (label, mut v) in [
                            ("queue", seen_q.clone()),
                            ("roster", seen_r.clone()),
                            ("windows", seen_w.clone()),
                        ] {
                            let before = v.len();
                            v.sort_unstable();
                            v.dedup();
                            assert_eq!(before, v.len(), "{label} index repeated: {c:?} h={h}");
                        }
                        assert!(seen_q.len() <= NEEDS_YOU_CAP, "{c:?} h={h}");
                        // A header always has at least one REAL row under
                        // it — never a bare `+N more`, which tells the user
                        // less than the single most-urgent row would.
                        if rows.contains(&SidebarRow::NeedsYouHeader) {
                            assert!(!seen_q.is_empty(), "{c:?} h={h} {rows:?}");
                        }
                        if rows.contains(&SidebarRow::SpacesHeader) {
                            assert!(!seen_r.is_empty(), "{c:?} h={h}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn hit_test_maps_rows_to_targets() {
        let rect = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 9,
        };
        let c = counts(0, 2, 0);
        // Row 0 is the `here` header: not a target.
        assert_eq!(hit_test(rect, c, 3, 0), None);
        // Name and branch rows of block 1 both select window 1.
        assert_eq!(hit_test(rect, c, 3, 3), Some(SidebarHit::Window(1)));
        assert_eq!(hit_test(rect, c, 3, 4), Some(SidebarHit::Window(1)));
        // Padding rows miss.
        assert_eq!(hit_test(rect, c, 3, 5), None);
        // Footer rows.
        assert_eq!(hit_test(rect, c, 3, 7), Some(SidebarHit::NewWindow));
        assert_eq!(hit_test(rect, c, 3, 8), Some(SidebarHit::Menu));
    }

    /// phux-k0cw: a queue row resolves to its own index (the dispatcher
    /// turns that into a local focus or a session switch), a roster row to
    /// its session, and both overflow rows to the fleet dashboard.
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
        assert_eq!(hit_test(rect, c, 3, 0), None, "header is inert");
        assert_eq!(hit_test(rect, c, 3, 1), Some(SidebarHit::NeedsYou(0)));
        assert_eq!(hit_test(rect, c, 3, 2), Some(SidebarHit::NeedsYou(1)));
        assert_eq!(hit_test(rect, c, 3, 4), None, "`here` header is inert");
        assert_eq!(hit_test(rect, c, 3, 5), Some(SidebarHit::Window(0)));

        // Roster rows.
        let tall = Rect { h: 12, ..rect };
        let c = counts(0, 1, 2);
        assert_eq!(hit_test(tall, c, 3, 4), None, "spaces header is inert");
        assert_eq!(hit_test(tall, c, 3, 5), Some(SidebarHit::Roster(0)));
        assert_eq!(hit_test(tall, c, 3, 6), Some(SidebarHit::Roster(1)));

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
        // Right-docked strip at x=60. Row 0 is the header; row 1 the
        // first window's name row.
        let rect = Rect {
            x: 60,
            y: 0,
            w: 20,
            h: 8,
        };
        let c = counts(0, 1, 0);
        assert_eq!(hit_test(rect, c, 60, 1), Some(SidebarHit::Window(0)));
        // The separator column (last column of the strip) is not a target
        // outside the chevron corner.
        assert_eq!(hit_test(rect, c, 79, 0), None);
        // Outside the strip entirely.
        assert_eq!(hit_test(rect, c, 59, 1), None);
        assert_eq!(hit_test(rect, c, 80, 1), None);
        assert_eq!(hit_test(rect, c, 60, 8), None);
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
            w: 24,
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
                    assert_eq!(hit, None);
                }
                SidebarRow::HereHeader => {
                    assert!(row_text(&buf, rect, y16).contains(HERE_HEADER));
                    assert_eq!(hit, None);
                }
                SidebarRow::SpacesHeader => {
                    assert!(row_text(&buf, rect, y16).contains(SPACES_HEADER));
                    assert_eq!(hit, None);
                }
                SidebarRow::WindowName(i) => {
                    assert!(row_text(&buf, rect, y16).contains(&windows[*i].name));
                    assert_eq!(hit, Some(SidebarHit::Window(*i)));
                }
                SidebarRow::WindowBranch(i) => {
                    assert_eq!(hit, Some(SidebarHit::Window(*i)));
                }
                SidebarRow::NeedsYou(j) => {
                    assert!(row_text(&buf, rect, y16).contains(&agents[*j].name));
                    assert_eq!(hit, Some(SidebarHit::NeedsYou(*j)));
                }
                SidebarRow::RosterEntry(j) => {
                    assert!(row_text(&buf, rect, y16).contains(&peers[*j].name));
                    assert_eq!(hit, Some(SidebarHit::Roster(*j)));
                }
                SidebarRow::NeedsYouOverflow | SidebarRow::RosterOverflow => {
                    assert!(row_text(&buf, rect, y16).contains(OVERFLOW_LABEL));
                    assert_eq!(hit, Some(SidebarHit::Fleet));
                }
                SidebarRow::Blank | SidebarRow::HereEmpty => {
                    assert_eq!(hit, None);
                }
                SidebarRow::NewWindow => {
                    assert!(row_text(&buf, rect, y16).contains(NEW_LABEL));
                    assert_eq!(hit, Some(SidebarHit::NewWindow));
                }
                SidebarRow::Menu => {
                    assert!(row_text(&buf, rect, y16).contains(MENU_LABEL));
                    assert_eq!(hit, Some(SidebarHit::Menu));
                }
            }
        }
    }
}
