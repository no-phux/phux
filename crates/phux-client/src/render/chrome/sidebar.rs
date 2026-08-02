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
use crate::render::overlay::help::HardcodedBinding;

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
/// The `spaces` section header (phux-foz.9) — herdr's word for the
/// window/workspace list.
pub const SPACES_HEADER: &str = "spaces";
/// The `agents` section header (phux-foz.9).
pub const AGENTS_HEADER: &str = "agents";
/// Empty-state placeholder for the `spaces` section (phux-foz.13) — shown
/// in place of window blocks when there are no windows to list.
pub const SPACES_EMPTY: &str = "no spaces";
/// Empty-state placeholder for the `agents` section (phux-foz.13).
///
/// Shown under the `agents` header when no pane is running a declared or
/// heuristically-identified agent, so the section reads as composed rather
/// than vanishing.
pub const AGENTS_EMPTY: &str = "no agents";
/// The collapse chevron painted in the strip's bottom corner
/// (phux-foz.9). Clicking it runs `toggle-sidebar`.
pub const COLLAPSE_GLYPH: &str = "‹";

/// The sidebar's click-target table for the help overlay's
/// `Mouse & menus` section (phux-i0e8.10.3). COLOCATED with [`hit_test`]
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
    /// Index of the window holding the agent's pane (its `select-window`
    /// index) — clicking the row jumps there.
    pub window: usize,
    /// The window's stored name, herdr's "workspace" column on the row.
    pub window_name: String,
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

/// One row of the strip, top to bottom. Both the painter and [`hit_test`]
/// derive from this single model, so paint and click targets cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarRow {
    /// The muted `spaces` section header.
    SpacesHeader,
    /// Window `i`'s name row.
    WindowName(usize),
    /// Window `i`'s branch row (dim; blank when the window has no branch).
    WindowBranch(usize),
    /// The muted `agents` section header.
    AgentsHeader,
    /// Agent entry `j`'s row (glyph + window name + `state - name`).
    Agent(usize),
    /// The `spaces` section's empty-state placeholder (phux-foz.13): a
    /// quiet `no spaces` line when there are no windows to list.
    SpacesEmpty,
    /// The `agents` section's empty-state placeholder (phux-foz.13): a
    /// quiet `no agents` line so the section reads as deliberately present
    /// rather than silently omitted.
    AgentsEmpty,
    /// Unused padding (section gap, or fill above the footer).
    Blank,
    /// The `+ new` affordance (create a window).
    NewWindow,
    /// The `= menu` affordance (open the command palette).
    Menu,
}

/// The interactive target a mouse position resolves to (phux-fce4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarHit {
    /// A window block or an agent row — clicking selects window `i` (its
    /// `select-window` index). Both rows of a block hit; an agent row
    /// resolves to the window holding its pane.
    Window(usize),
    /// The `+ new` affordance.
    NewWindow,
    /// The `= menu` affordance.
    Menu,
    /// The collapse chevron in the bottom corner (phux-foz.9) —
    /// clicking runs `toggle-sidebar`.
    Collapse,
}

/// The strip's row model for `window_count` windows and `agent_count`
/// agent entries in an `h`-row rect.
///
/// Top to bottom: the `spaces` header (with a `no spaces` placeholder when
/// there are no windows), then a fixed two-row block (name + branch) per
/// window; when there is room for a blank gap, the `agents` header, and at
/// least one row, the agents section follows — one row per entry, or a
/// single `no agents` empty-state row when there are none (phux-foz.13), so
/// the section is deliberately present rather than silently dropped. When
/// `h >= MIN_FOOTER_HEIGHT` the bottom two rows are
/// reserved for the `+ new` / `= menu` affordances and body rows that
/// would collide are truncated. Fixed-size blocks keep the model derivable
/// from the *counts* alone, which is what lets the input dispatcher
/// hit-test without rebuilding the full window projection.
#[must_use]
pub fn row_model(window_count: usize, agent_count: usize, h: u16) -> Vec<SidebarRow> {
    let h = usize::from(h);
    let footer = if h >= usize::from(MIN_FOOTER_HEIGHT) {
        2
    } else {
        0
    };
    let body = h - footer;
    let mut rows = Vec::with_capacity(h);
    if body > 0 {
        rows.push(SidebarRow::SpacesHeader);
    }
    // phux-foz.13: an empty `spaces` section shows a quiet placeholder
    // rather than jumping straight to the agents header — the sidebar
    // reads as two composed sections, not a bare label.
    if window_count == 0 && rows.len() < body {
        rows.push(SidebarRow::SpacesEmpty);
    }
    'blocks: for i in 0..window_count {
        for row in [SidebarRow::WindowName(i), SidebarRow::WindowBranch(i)] {
            if rows.len() >= body {
                break 'blocks;
            }
            // A truncated block may show a name row without its branch
            // row — a dangling name is still more useful than a blank.
            rows.push(row);
        }
    }
    // The agents section renders whenever its gap + header + one row all
    // fit — a bare header with no rows is noise, but phux-foz.13 fills that
    // row with a `no agents` placeholder when there are no entries so the
    // section is deliberately present rather than silently omitted.
    if rows.len() + 3 <= body {
        rows.push(SidebarRow::Blank);
        rows.push(SidebarRow::AgentsHeader);
        if agent_count == 0 {
            rows.push(SidebarRow::AgentsEmpty);
        } else {
            for j in 0..agent_count {
                if rows.len() >= body {
                    break;
                }
                rows.push(SidebarRow::Agent(j));
            }
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
/// column, or a blank row). `window_count` must be the same list length
/// the painter was fed; `agent_windows` is the per-agent-row window index
/// (display order), so an agent row resolves to the window holding its
/// pane. The bottom corner cell — on the separator column, which is
/// otherwise never a target — is the collapse chevron (phux-foz.9).
#[must_use]
pub fn hit_test(
    rect: Rect,
    window_count: usize,
    agent_windows: &[usize],
    x: u16,
    y: u16,
) -> Option<SidebarHit> {
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
    match row_model(window_count, agent_windows.len(), rect.h).get(row)? {
        SidebarRow::WindowName(i) | SidebarRow::WindowBranch(i) => Some(SidebarHit::Window(*i)),
        SidebarRow::Agent(j) => agent_windows.get(*j).map(|w| SidebarHit::Window(*w)),
        SidebarRow::NewWindow => Some(SidebarHit::NewWindow),
        SidebarRow::Menu => Some(SidebarHit::Menu),
        SidebarRow::SpacesHeader
        | SidebarRow::AgentsHeader
        | SidebarRow::SpacesEmpty
        | SidebarRow::AgentsEmpty
        | SidebarRow::Blank => None,
    }
}

/// VT painter for the window sidebar.
#[derive(Debug)]
pub struct SidebarPainter {
    windows: Vec<WindowInfo>,
    agents: Vec<AgentEntry>,
    theme: Theme,
    /// Cache: the `(rect, windows, agents)` of the last paint. An
    /// identical repaint is a zero-byte no-op.
    last: Option<(Rect, Vec<WindowInfo>, Vec<AgentEntry>)>,
}

impl SidebarPainter {
    /// A painter styled by `theme`, initially showing no windows.
    #[must_use]
    pub const fn new(theme: Theme) -> Self {
        Self {
            windows: Vec::new(),
            agents: Vec::new(),
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

    /// Replace the agents-section entries (phux-foz.9). Same change-report
    /// contract as [`Self::set_windows`].
    pub fn set_agents(&mut self, agents: Vec<AgentEntry>) -> bool {
        if self.agents == agents {
            return false;
        }
        self.agents = agents;
        true
    }

    /// The per-agent-row window index, in display order — the mapping
    /// [`hit_test`] needs to resolve an agent row to its window.
    #[must_use]
    pub fn agent_windows(&self) -> Vec<usize> {
        self.agents.iter().map(|e| e.window).collect()
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
        if self
            .last
            .as_ref()
            .is_some_and(|(r, w, a)| *r == rect && *w == self.windows && *a == self.agents)
        {
            return Ok(());
        }
        let buf = self.compose(rect);
        emit(out, &buf, rect)?;
        self.last = Some((rect, self.windows.clone(), self.agents.clone()));
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
        let win_budget = avail
            .saturating_sub(state_text.chars().count() + 1)
            .max(avail.min(5));
        let win_label = truncate(
            &e.window_name,
            win_budget.min(e.window_name.chars().count()),
        );
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
        let model = row_model(self.windows.len(), self.agents.len(), rect.h);
        if text_w > 0 {
            let lines: Vec<Line<'static>> = model
                .iter()
                .map(|row| match row {
                    SidebarRow::SpacesHeader => self.header_line(SPACES_HEADER, text_w),
                    SidebarRow::AgentsHeader => self.header_line(AGENTS_HEADER, text_w),
                    SidebarRow::WindowName(i) => self
                        .windows
                        .get(*i)
                        .map_or_else(|| Line::from(""), |w| self.name_line(w, text_w)),
                    SidebarRow::WindowBranch(i) => self
                        .windows
                        .get(*i)
                        .map_or_else(|| Line::from(""), |w| self.branch_line(w, text_w)),
                    SidebarRow::Agent(j) => self
                        .agents
                        .get(*j)
                        .map_or_else(|| Line::from(""), |e| self.agent_line(e, text_w)),
                    SidebarRow::SpacesEmpty => self.empty_line(SPACES_EMPTY, text_w),
                    SidebarRow::AgentsEmpty => self.empty_line(AGENTS_EMPTY, text_w),
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

/// Truncate `s` to `max` cells, appending `…` when it overflows. A `max` of
/// 0 yields the empty string.
fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_owned();
    }
    s.chars()
        .take(max.saturating_sub(1))
        .chain(std::iter::once('…'))
        .collect()
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
            window,
            window_name: window_name.to_owned(),
            name: name.to_owned(),
            state,
            attention: false,
            seen: false,
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

        let mut unseen = agent(0, "a", "claude", AgentMetaState::Done);
        unseen.seen = false;
        p.set_agents(vec![unseen.clone()]);
        let row = row_text(&p.compose_buffer(rect), rect, 5);
        assert!(row.contains('◆'), "unreviewed done: {row:?}");

        let seen = AgentEntry {
            seen: true,
            ..unseen
        };
        p.set_agents(vec![seen]);
        let row = row_text(&p.compose_buffer(rect), rect, 5);
        assert!(row.contains('○'), "reviewed done relaxes: {row:?}");

        p.set_agents(vec![agent(0, "a", "claude", AgentMetaState::Working)]);
        let row = row_text(&p.compose_buffer(rect), rect, 5);
        assert!(row.contains('◐'), "working: {row:?}");
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
        // phux-foz.9: the spaces header tops the strip.
        assert!(plain.contains(SPACES_HEADER), "spaces header: {plain:?}");
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
        p.set_agents(vec![agent(0, "b", "claude", AgentMetaState::Idle)]);
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
        assert!(p.set_agents(vec![idle.clone()]));
        assert!(!p.set_agents(vec![idle]));
        assert!(p.set_agents(vec![agent(0, "a", "claude", AgentMetaState::Working)]));
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
            row_text(&buf, rect, 0).contains(SPACES_HEADER),
            "row 0 is the spaces header: {:?}",
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
        p.set_agents(vec![
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
        // Rows: 0 spaces header, 1-4 window blocks, 5 gap, 6 agents
        // header, 7-8 agent rows.
        assert!(row_text(&buf, rect, 5).trim().is_empty());
        assert!(
            row_text(&buf, rect, 6).contains(AGENTS_HEADER),
            "agents header: {:?}",
            row_text(&buf, rect, 6)
        );
        let claude_row = row_text(&buf, rect, 7);
        assert!(
            claude_row.contains("phux") && claude_row.contains("idle - claude"),
            "agent row shows window + state - name: {claude_row:?}"
        );
        let worker_row = row_text(&buf, rect, 8);
        assert!(
            worker_row.contains("working - merge-queue-w5"),
            "second agent row: {worker_row:?}"
        );
    }

    /// phux-foz.13: with windows but no agent-running panes, the agents
    /// section still renders its header plus a quiet `no agents` empty-state
    /// line (dim + italic), instead of vanishing — the strip reads as two
    /// deliberate sections. The placeholder is inert (not a click target).
    #[test]
    fn empty_agents_section_shows_a_placeholder() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![win_branch("phux", true, "main")]);
        // No agents set.
        let rect = Rect {
            x: 0,
            y: 0,
            w: 24,
            h: 12,
        };
        let buf = p.compose_buffer(rect);
        // Rows: 0 spaces header, 1 name, 2 branch, 3 gap, 4 agents header,
        // 5 `no agents` placeholder.
        assert!(
            row_text(&buf, rect, 4).contains(AGENTS_HEADER),
            "agents header present even with no agents: {:?}",
            row_text(&buf, rect, 4)
        );
        assert!(
            row_text(&buf, rect, 5).contains(AGENTS_EMPTY),
            "empty agents section shows a placeholder: {:?}",
            row_text(&buf, rect, 5)
        );
        // The placeholder row is not a click target.
        assert_eq!(hit_test(rect, 1, &[], 3, 5), None);
    }

    /// phux-foz.13: an empty `spaces` section (no windows at all) shows its
    /// own `no spaces` placeholder rather than leaping straight to the
    /// agents header.
    #[test]
    fn empty_spaces_section_shows_a_placeholder() {
        let p = SidebarPainter::new(Theme::default());
        // No windows, no agents.
        let rect = Rect {
            x: 0,
            y: 0,
            w: 24,
            h: 12,
        };
        let buf = p.compose_buffer(rect);
        assert!(
            row_text(&buf, rect, 0).contains(SPACES_HEADER),
            "spaces header tops the strip: {:?}",
            row_text(&buf, rect, 0)
        );
        assert!(
            row_text(&buf, rect, 1).contains(SPACES_EMPTY),
            "empty spaces section shows a placeholder: {:?}",
            row_text(&buf, rect, 1)
        );
        assert_eq!(hit_test(rect, 0, &[], 3, 1), None, "placeholder is inert");
    }

    /// phux-foz.9: the full sectioned layout, pinned as a snapshot — the
    /// herdr-parity shape (spaces header, dotted window blocks with branch
    /// sub-lines, agents section, bottom-anchored affordances + collapse
    /// chevron) must not drift silently.
    #[test]
    fn sectioned_layout_snapshot() {
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(vec![
            win_branch("phux", true, "main"),
            win("scratch", false),
        ]);
        p.set_agents(vec![
            agent(0, "phux", "claude", AgentMetaState::Idle),
            agent(1, "scratch", "codex", AgentMetaState::Blocked),
        ]);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 24,
            h: 16,
        };
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
        insta::assert_snapshot!(out);
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
        for binding in HELP_BINDINGS {
            match binding.chord {
                "click" => {
                    assert_eq!(
                        hit_test(rect, 1, &[], 3, 1),
                        Some(SidebarHit::Window(0)),
                        "a window row click selects that window"
                    );
                }
                NEW_LABEL => {
                    assert_eq!(hit_test(rect, 1, &[], 3, 6), Some(SidebarHit::NewWindow));
                }
                MENU_LABEL => {
                    assert_eq!(hit_test(rect, 1, &[], 3, 7), Some(SidebarHit::Menu));
                }
                COLLAPSE_GLYPH => {
                    assert_eq!(hit_test(rect, 1, &[], 19, 7), Some(SidebarHit::Collapse));
                }
                other => panic!(
                    "help table row `{other}` has no adjacency check — \
                     add one that drives hit_test"
                ),
            }
        }
    }

    // ---------- phux-fce4 / phux-foz.9: row model + hit-test ----------

    #[test]
    fn row_model_reserves_footer_and_truncates_blocks() {
        // 3 windows in 9 rows: header + 6 window-area rows fit 3 blocks.
        let rows = row_model(3, 0, 9);
        assert_eq!(rows.len(), 9);
        assert_eq!(rows[0], SidebarRow::SpacesHeader);
        assert_eq!(rows[1], SidebarRow::WindowName(0));
        assert_eq!(rows[2], SidebarRow::WindowBranch(0));
        assert_eq!(rows[5], SidebarRow::WindowName(2));
        assert_eq!(rows[6], SidebarRow::WindowBranch(2));
        assert_eq!(rows[7], SidebarRow::NewWindow);
        assert_eq!(rows[8], SidebarRow::Menu);
        // 3 windows in 7 rows: 5 body rows truncate the third block.
        let rows = row_model(3, 0, 7);
        assert_eq!(rows[4], SidebarRow::WindowBranch(1));
        assert_eq!(rows[5], SidebarRow::NewWindow);
        assert_eq!(rows[6], SidebarRow::Menu);
        // Below the minimum height there is no footer.
        let rows = row_model(1, 0, 3);
        assert_eq!(
            rows,
            vec![
                SidebarRow::SpacesHeader,
                SidebarRow::WindowName(0),
                SidebarRow::WindowBranch(0),
            ]
        );
    }

    /// phux-foz.9: the agents section (gap + header + rows) appears after
    /// the window blocks when it fits, and is dropped whole when it can't
    /// show at least one entry row.
    #[test]
    fn row_model_places_the_agents_section() {
        // 1 window + 2 agents in 10 rows: header, block, gap, header, rows.
        let rows = row_model(1, 2, 10);
        assert_eq!(
            rows,
            vec![
                SidebarRow::SpacesHeader,
                SidebarRow::WindowName(0),
                SidebarRow::WindowBranch(0),
                SidebarRow::Blank,
                SidebarRow::AgentsHeader,
                SidebarRow::Agent(0),
                SidebarRow::Agent(1),
                SidebarRow::Blank,
                SidebarRow::NewWindow,
                SidebarRow::Menu,
            ]
        );
        // Too short for gap + header + one row: the section drops whole —
        // no dangling header.
        let rows = row_model(1, 2, 7);
        assert!(
            !rows.contains(&SidebarRow::AgentsHeader),
            "no room for an entry row => no header: {rows:?}"
        );
        // Agent rows beyond the body truncate.
        let rows = row_model(1, 5, 10);
        assert_eq!(rows[7], SidebarRow::Agent(2));
        assert_eq!(rows[8], SidebarRow::NewWindow);
    }

    #[test]
    fn hit_test_maps_rows_to_targets() {
        let rect = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 9,
        };
        // Row 0 is the spaces header: not a target.
        assert_eq!(hit_test(rect, 2, &[], 3, 0), None);
        // Name and branch rows of block 1 both select window 1.
        assert_eq!(hit_test(rect, 2, &[], 3, 3), Some(SidebarHit::Window(1)));
        assert_eq!(hit_test(rect, 2, &[], 3, 4), Some(SidebarHit::Window(1)));
        // Padding rows miss.
        assert_eq!(hit_test(rect, 2, &[], 3, 5), None);
        // Footer rows.
        assert_eq!(hit_test(rect, 2, &[], 3, 7), Some(SidebarHit::NewWindow));
        assert_eq!(hit_test(rect, 2, &[], 3, 8), Some(SidebarHit::Menu));
    }

    /// phux-foz.9: an agent row resolves to the window holding its pane,
    /// and the agents header is inert.
    #[test]
    fn hit_test_maps_agent_rows_to_their_windows() {
        let rect = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 10,
        };
        // 1 window + 2 agents: rows 3 gap, 4 agents header, 5-6 agents.
        let agents = [0usize, 0];
        assert_eq!(hit_test(rect, 1, &agents, 3, 4), None, "header is inert");
        assert_eq!(
            hit_test(rect, 1, &agents, 3, 5),
            Some(SidebarHit::Window(0))
        );
        assert_eq!(
            hit_test(rect, 1, &agents, 3, 6),
            Some(SidebarHit::Window(0))
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
        assert_eq!(hit_test(rect, 1, &[], 19, 7), Some(SidebarHit::Collapse));
        // The rest of the separator column stays inert.
        assert_eq!(hit_test(rect, 1, &[], 19, 6), None);
        assert_eq!(hit_test(rect, 1, &[], 19, 0), None);
        // No footer (short strip) => no chevron target.
        let short = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 3,
        };
        assert_eq!(hit_test(short, 1, &[], 19, 2), None);
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
        assert_eq!(hit_test(rect, 1, &[], 60, 1), Some(SidebarHit::Window(0)));
        // The separator column (last column of the strip) is not a target
        // outside the chevron corner.
        assert_eq!(hit_test(rect, 1, &[], 79, 0), None);
        // Outside the strip entirely.
        assert_eq!(hit_test(rect, 1, &[], 59, 1), None);
        assert_eq!(hit_test(rect, 1, &[], 80, 1), None);
        assert_eq!(hit_test(rect, 1, &[], 60, 8), None);
        // Degenerate rects never hit.
        assert_eq!(
            hit_test(
                Rect {
                    x: 0,
                    y: 0,
                    w: 0,
                    h: 0
                },
                1,
                &[],
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
        let agent_windows: Vec<usize> = agents.iter().map(|e| e.window).collect();
        let mut p = SidebarPainter::new(Theme::default());
        p.set_windows(windows.clone());
        p.set_agents(agents.clone());
        let buf = p.compose_buffer(rect);
        for (y, row) in row_model(windows.len(), agents.len(), rect.h)
            .iter()
            .enumerate()
        {
            let y16 = u16::try_from(y).expect("row fits u16");
            let hit = hit_test(rect, windows.len(), &agent_windows, 2, y16);
            match row {
                SidebarRow::SpacesHeader => {
                    assert!(row_text(&buf, rect, y16).contains(SPACES_HEADER));
                    assert_eq!(hit, None);
                }
                SidebarRow::AgentsHeader => {
                    assert!(row_text(&buf, rect, y16).contains(AGENTS_HEADER));
                    assert_eq!(hit, None);
                }
                SidebarRow::WindowName(i) => {
                    assert!(row_text(&buf, rect, y16).contains(&windows[*i].name));
                    assert_eq!(hit, Some(SidebarHit::Window(*i)));
                }
                SidebarRow::WindowBranch(i) => {
                    assert_eq!(hit, Some(SidebarHit::Window(*i)));
                }
                SidebarRow::Agent(j) => {
                    assert!(row_text(&buf, rect, y16).contains(&agents[*j].name));
                    assert_eq!(hit, Some(SidebarHit::Window(agents[*j].window)));
                }
                SidebarRow::Blank | SidebarRow::SpacesEmpty | SidebarRow::AgentsEmpty => {
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
