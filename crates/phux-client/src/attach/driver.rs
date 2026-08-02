//! The attach loop driver: connect, HELLO + ATTACH, then `tokio::select!`
//! over the server, stdin, SIGWINCH, and the detach chord until the server
//! sends `DETACHED` or the user requests detach.
//!
//! The driver owns:
//!
//! * the [`super::connection::Connection`] (UDS transport),
//! * stdout via a [`RawModeGuard`] that flips the outer terminal into raw
//!   mode + alt screen on construction and restores it on drop (panic-safe
//!   per ADR-0003's "no hung outer terminals" requirement),
//! * a stdin reader,
//! * a SIGWINCH listener (currently a no-op; once `VIEWPORT_RESIZE` lands
//!   in phux-4hp it will start sending resize frames upstream),
//! * a local `libghostty_vt::Terminal` + [`TerminalRenderer`] for the focused
//!   pane (under ADR-0013 the client is bytes-in / `vt_write` / dirty-row
//!   redraw — see `research/2026-05-25-libghostty-renderstate.md`).

#![allow(
    clippy::result_large_err,
    reason = "AttachError carries an io::Error which is naturally large; the variants are mutually exclusive and we never carry the result in a hot loop."
)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};
use std::os::fd::AsFd;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use libghostty_vt::terminal::Mode;
use libghostty_vt::{Terminal as GhosttyTerminal, TerminalOptions};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, Layer, LayerSet, OutputMode, detect_color_support};
use phux_protocol::ids::{ClientId, TerminalId};
use phux_protocol::wire::frame::{
    AttachTarget, CONFIG_RELOAD_KEY, Command, FrameKind, Scope, TerminalLifecycle, ViewportInfo,
};
use rustix::termios::{LocalModes, OptionalActions, Termios};
use tokio::io::AsyncReadExt;
use tokio::signal::unix::{SignalKind, signal};
use tracing::Instrument as _;

use super::actions::{PendingSplit, PendingWindow};
use super::connection::{Connection, Dial};
use super::exec_widgets::spawn_exec_feed_runners;
use super::input::StdinParser;
use super::input_dispatch::{
    DispatchCtx, ReattachTarget, dispatch_input_events, encode_layout_or_log,
};
use super::paint::{
    SidebarEdge, SidebarReservation, content_rect, paint_bar_after_pane, paint_chrome_in_place,
    paint_full_frame,
};
use super::plugin_actions::{self, PluginActionEntry, PluginRunResult};
use super::plugin_panes;
use super::record::{SessionRecorder, TeeSink};
use super::render::{SelectionRect, TerminalRenderer, write_cup, write_reset};
use super::repaint::{RepaintAccumulator, RepaintLevel};
use super::server_frame::{AgentMetaIndex, handle_server_frame};
use crate::agent_meta::{
    AgentAttention, AgentMetaState, AgentRecord, TERMINAL_AGENT_KEY, agent_name_from_title,
    parse_agent_record,
};
use crate::layout::Workspace;
pub(super) use crate::layout_ops::{
    DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, LAYOUT_KEY, layout_key,
};
use crate::predict::{Overlay, PredictionState, PredictiveConfig};
use crate::render::chrome::sidebar::{AgentEntry, SidebarPainter, attention_rank};
use crate::render::chrome::status_bar::StatusBarPainter;
use crate::render::overlay::OverlayState;
use phux_config::SidebarPosition;

/// Driver-owned state for client-local attention navigation (phux-oih5.16).
///
/// The first jump saves the pane the user came from. Further cycling leaves
/// that origin untouched; return consumes it even when the pane has gone
/// stale. Nothing in this state is serialized or written to metadata.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct AttentionNavigation {
    origin: Option<TerminalId>,
}

impl AttentionNavigation {
    /// Save an origin only when a navigation excursion is not already active.
    pub(super) fn save_origin_once(&mut self, origin: Option<&TerminalId>) {
        if self.origin.is_none() {
            self.origin = origin.cloned();
        }
    }

    /// Consume the saved origin. A stale origin must not remain armed forever.
    pub(super) const fn take_origin(&mut self) -> Option<TerminalId> {
        self.origin.take()
    }
}

/// One pane's mirror: the libghostty Terminal that ingests
/// `TERMINAL_OUTPUT` and the renderer that paints it to the outer
/// terminal. Grown from "one of these per attach" (single-pane v0) to
/// "one of these per leaf in the layout tree" by phux-4li.4. The driver
/// keeps a `PaneMap` of these keyed by [`TerminalId`].
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent per-pane flags (scroll, attention, sync-output, seen); a bitset would obscure every read site"
)]
pub(super) struct PaneSlot {
    /// libghostty mirror for this pane.
    pub terminal: GhosttyTerminal<'static, 'static>,
    /// Cached render scaffolding. One per pane so libghostty's iterators
    /// stay warm across frames (the renderer's `last_cursor` is also
    /// per-pane, so each pane's predictive-echo anchor is independent).
    pub renderer: TerminalRenderer<'static>,
    /// ADR-0033 supervisory lifecycle for this pane, driven by inbound
    /// `TerminalControl` events: `Running` until a `Freeze` (SIGSTOP) flips it
    /// to `Frozen`. Read at paint time to render the "FROZEN" chrome badge.
    pub lifecycle: TerminalLifecycle,
    /// ADR-0033 input-lease holder for this pane (the wire `ClientId` that has
    /// "the wheel"), or `None` when the pane is `Open`. Compared against the
    /// driver's own `ClientId` to render "you" vs another client.
    pub input_holder: Option<ClientId>,
    /// `true` while the client-local viewport is (possibly) scrolled up into
    /// scrollback — set by wheel / copy-mode scrolls, cleared when a key press
    /// headed for the pane snaps the viewport back to the live screen (tmux
    /// behavior). Without the snap, a scrolled viewport stays pinned in
    /// scrollback forever and the pane looks frozen: new output (e.g. the
    /// shell prompt after a TUI app exits) lands below the visible rows.
    pub viewport_scrolled: bool,
    /// phux-foz.1: `true` when an agent in this pane is waiting on a human
    /// answer. Set by an inbound ADR-0035 `AgentEvent::Asked`; cleared when
    /// the user sends key/paste input to the pane (see
    /// [`clear_attention_on_input`]). Read at chrome-paint time for the
    /// window tab marker and the status-bar attention hint.
    pub attention: bool,
    /// Start of the current DEC synchronized-output transaction (`?2026h`).
    pub sync_output_since: Option<tokio::time::Instant>,
    /// Whether mirror state changed during the transaction.
    pub sync_output_dirty: bool,
    /// phux-foz.4: the pane's working directory as the server last
    /// announced it — seeded from the `ATTACHED` snapshot's
    /// `TerminalInfo.cwd` (the spawn cwd) and refined by `cwd_changed`
    /// events. `None` until either lands. Projected into the status-bar
    /// `cwd` widget when this pane is focused.
    pub cwd: Option<String>,
    /// phux-foz.4: exit code of the last command that finished in this
    /// pane (`command_finished.exit_code`, OSC-133 `D` mark). `None`
    /// before the first command finishes or when the shell reported no
    /// code. Projected into the status-bar `exit` widget when focused.
    pub last_exit: Option<i32>,
    /// phux-foz.9: the OSC 0/2 title as of the last chrome-relevant VT
    /// apply, mirrored out of [`Self::terminal`] so
    /// [`Self::title_changed`] can detect a title transition. The title
    /// is the ONLY identity signal a plain `claude`/`codex` pane emits
    /// (no `phux.agent/v1` record, no ADR-0035 events), and it arrives
    /// as ordinary `TERMINAL_OUTPUT` bytes — without this diff the
    /// sidebar's agents section (and the window-tab labels, phux-efj7)
    /// would only refresh on an unrelated chrome event. Empty ⇒ no
    /// title set, matching libghostty's `title()` contract.
    pub last_title: String,
    /// The attention ladder's "have you looked at this?" bit. Set whenever the
    /// pane is the focused one (every loop iteration), cleared whenever an
    /// UNFOCUSED pane's `phux.agent/v1` record changes.
    ///
    /// This is what lets the sidebar rank "finished, unread" above "still
    /// working": an agent that goes `done` in a background pane re-arms as
    /// unseen and climbs the strip until the user actually visits it. Starts
    /// `false` — a pane you have never focused has never been reviewed.
    pub seen: bool,
}

impl std::fmt::Debug for PaneSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaneSlot").finish_non_exhaustive()
    }
}

impl PaneSlot {
    /// Allocate a fresh slot at a known pane size.
    ///
    /// The client must size libghostty before feeding VT bytes: wrapping,
    /// clipping, absolute cursor movement, and several style spans are all
    /// width-sensitive during `vt_write`, so a later resize cannot reliably
    /// recover from a placeholder geometry.
    pub(super) fn new_with_size(cols: u16, rows: u16) -> Result<Self, AttachError> {
        let mut terminal = GhosttyTerminal::new(TerminalOptions {
            cols: cols.max(1),
            rows: rows.max(1),
            max_scrollback: 10_000,
        })?;
        phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut terminal)?;
        terminal.resize(
            cols.max(1),
            rows.max(1),
            super::paint::FALLBACK_CELL_PX.0,
            super::paint::FALLBACK_CELL_PX.1,
        )?;
        Ok(Self {
            terminal,
            renderer: TerminalRenderer::new()?,
            // Panes start running and un-leased; a late-arriving
            // TerminalControl (after the pane's first snapshot/output) updates
            // these — a pane that exists before its control state is benign.
            lifecycle: TerminalLifecycle::Running,
            input_holder: None,
            viewport_scrolled: false,
            attention: false,
            sync_output_since: None,
            sync_output_dirty: false,
            cwd: None,
            last_exit: None,
            last_title: String::new(),
            seen: false,
        })
    }

    /// Allocate a fresh slot with a conservative placeholder size.
    /// Prefer [`Self::new_with_size`] whenever the attach snapshot,
    /// viewport, or layout already tells us the pane's real dimensions.
    pub(super) fn new() -> Result<Self, AttachError> {
        Self::new_with_size(80, 24)
    }

    /// phux-foz.9: whether the pane's OSC 0/2 title moved since the last
    /// call, updating the [`Self::last_title`] mirror. Called after every
    /// `vt_write` on the content-frame paths (`TERMINAL_OUTPUT` /
    /// `TERMINAL_SNAPSHOT`), whose outcome sets `chrome_dirty` on `true`:
    /// window-tab labels and the sidebar's agents section both derive
    /// from the title (see `window_infos` / `agent_entries`), and title
    /// bytes flow in ordinary output frames that otherwise never trigger
    /// a chrome refresh — a plain `claude` pane's row would only appear
    /// (and, after exit, disappear) on an unrelated event. The compare is
    /// a length-bounded `str` equality against the mirror, so the
    /// steady-state per-frame cost is negligible next to the `vt_write`
    /// that precedes it.
    pub(super) fn title_changed(&mut self) -> bool {
        let current = self.terminal.title().unwrap_or_default();
        if self.last_title == current {
            return false;
        }
        self.last_title = current.to_owned();
        true
    }

    /// Refresh synchronized-output bookkeeping after a VT write. Returns
    /// `true` while painting must remain suppressed.
    pub(super) fn update_sync_output(&mut self, now: tokio::time::Instant) -> bool {
        let active = self.terminal.mode(Mode::SYNC_OUTPUT).unwrap_or(false);
        if active {
            self.sync_output_since.get_or_insert(now);
            self.sync_output_dirty = true;
        } else {
            self.sync_output_since = None;
            self.sync_output_dirty = false;
        }
        active
    }
}

/// ADR-0033: compose the status-bar supervisory badge for the focused pane,
/// or `None` when it is running and un-leased (so no badge paints). Reads the
/// per-pane lifecycle + input-lease holder tracked from `TerminalControl`
/// events; the holder renders as "you" when it matches this client's own id,
/// else as the other client's numeric id. No emojis (plain ASCII chrome).
fn supervisory_badge(
    panes: &HashMap<TerminalId, PaneSlot>,
    focused_pane: Option<&TerminalId>,
    own_client_id: Option<ClientId>,
) -> Option<String> {
    let slot = panes.get(focused_pane?)?;
    let frozen = matches!(slot.lifecycle, TerminalLifecycle::Frozen);
    format_supervisory_badge(frozen, slot.input_holder, own_client_id)
}

/// Pure badge formatter (split out from [`supervisory_badge`] so the
/// state→string mapping is testable without a libghostty-backed `PaneSlot`).
/// `None` ⇒ no badge (running and un-leased).
fn format_supervisory_badge(
    frozen: bool,
    input_holder: Option<ClientId>,
    own_client_id: Option<ClientId>,
) -> Option<String> {
    let wheel = input_holder.map(|holder| {
        if Some(holder) == own_client_id {
            "WHEEL:you".to_owned()
        } else {
            format!("WHEEL:c{}", holder.get())
        }
    });
    match (frozen, wheel) {
        (false, None) => None,
        (true, None) => Some("[ FROZEN ]".to_owned()),
        (false, Some(w)) => Some(format!("[ {w} ]")),
        (true, Some(w)) => Some(format!("[ FROZEN {w} ]")),
    }
}

/// phux-foz.1: compose the status-bar attention hint, or `None` when no pane
/// is waiting on a human answer. Counts every pane with the ADR-0035 asked
/// flag set (across ALL windows, not just the active one — the hint's job is
/// to surface a question the user cannot currently see).
fn attention_hint(panes: &HashMap<TerminalId, PaneSlot>) -> Option<String> {
    format_attention_hint(panes.values().filter(|slot| slot.attention).count())
}

/// Pure hint formatter (split out from [`attention_hint`] so the count→string
/// mapping is testable without a libghostty-backed `PaneSlot`). `None` ⇒ no
/// hint (nothing is asking). Plain ASCII chrome, matching the ADR-0033
/// supervisory badge convention.
fn format_attention_hint(asking: usize) -> Option<String> {
    match asking {
        0 => None,
        1 => Some("[ ASK ]".to_owned()),
        n => Some(format!("[ ASK x{n} ]")),
    }
}

/// phux-p4vp: the driver's per-pane workspace metadata — each pane's
/// working directory (from the `ATTACHED` snapshot's `TerminalInfo::cwd`)
/// plus the memoizing branch cache that turns a cwd into a VCS branch
/// label by reading `.git/HEAD` (see [`crate::vcs`]). Entirely
/// client-local: nothing here touches the wire or the server's actor
/// path, and lookups are cached file reads, never a `git` subprocess.
#[derive(Debug, Default)]
pub(super) struct VcsIndex {
    /// Pane → working directory, seeded from the `ATTACHED` snapshot.
    cwds: HashMap<TerminalId, std::path::PathBuf>,
    /// cwd → branch memo.
    cache: crate::vcs::BranchCache,
}

impl VcsIndex {
    /// Fold an `ATTACHED` snapshot's `(pane, cwd)` pairs into the index.
    /// The snapshot is authoritative for the panes it names; panes that no
    /// longer exist are dropped (re-attach hygiene).
    pub(super) fn apply_snapshot(&mut self, pane_cwds: Vec<(TerminalId, String)>) {
        if pane_cwds.is_empty() {
            return;
        }
        self.cwds = pane_cwds
            .into_iter()
            .map(|(id, cwd)| (id, std::path::PathBuf::from(cwd)))
            .collect();
    }

    /// The VCS branch label for `pane`'s working directory, or `None` when
    /// the cwd is unknown or not inside a repository.
    pub(super) fn branch_for_pane(&mut self, pane: &TerminalId) -> Option<String> {
        let cwd = self.cwds.get(pane)?.clone();
        self.cache.branch_for(&cwd)
    }

    /// phux-foz.7: the VCS branch label for an explicit `cwd` (the fleet
    /// dashboard resolves against the pane's *live* cwd — snapshot-seeded
    /// and refined by `cwd_changed` events — rather than this index's
    /// snapshot-only map). Same memoized `.git/HEAD` read, never a
    /// subprocess.
    pub(super) fn branch_for_cwd(&mut self, cwd: &str) -> Option<String> {
        self.cache.branch_for(std::path::Path::new(cwd))
    }
}

/// Refresh the window strip AND the supervisory badge together (ADR-0033),
/// plus the phux-foz.1 attention hint.
///
/// All three feed one status-bar paint, so they must stay in lockstep: a site
/// that refreshed the window list on a focus/layout change but forgot the
/// badge would silently desync them. This single chokepoint makes that
/// impossible — every focus/layout-change site calls it instead of
/// hand-rolling the trio.
///
/// Returns `true` when any painter input actually changed, so a caller that
/// paints nothing else (the `chrome_dirty` event path) can gate its repaint
/// on it instead of repainting the full frame for state the user already
/// sees.
#[allow(
    clippy::too_many_arguments,
    reason = "arg list mirrors the driver's chrome state; the ADR-0040 agent index made it 8 and the phux-p4vp vcs index 9"
)]
fn refresh_window_chrome(
    status_bar: Option<&mut StatusBarPainter>,
    sidebar_painter: &mut SidebarPainter,
    workspace: &Workspace,
    panes: &HashMap<TerminalId, PaneSlot>,
    focused_pane: Option<&TerminalId>,
    zoomed: Option<&TerminalId>,
    own_client_id: Option<ClientId>,
    // ADR-0040: structured `phux.agent/v1` records; a window whose focused
    // leaf carries one is labelled from it instead of the OSC title. The whole
    // index (not just `records`) because the sidebar's agent rows are ORDERED
    // by the attention ladder, whose tiebreak is the index's per-pane
    // last-change clock.
    agent_meta: &AgentMetaIndex,
    // phux-p4vp: pane cwd + branch memo; each window's branch line derives
    // from its focused leaf's working directory.
    vcs: &mut VcsIndex,
) -> bool {
    let windows = window_infos(workspace, panes, zoomed, &agent_meta.records, vcs);
    let mut changed = false;
    if let Some(sb) = status_bar {
        changed |= sb.set_windows(windows.clone());
        changed |= sb.set_supervisory(supervisory_badge(panes, focused_pane, own_client_id));
        changed |= sb.set_attention(attention_hint(panes));
        // phux-foz.4: project the focused pane's data feeds into the bar so
        // the `cwd` / `exit` widgets track focus changes and inbound
        // `cwd_changed` / `command_finished` events through this same
        // chokepoint. Unfocused (or unknown) folds to None => the widgets
        // render nothing.
        let focused = focused_pane.and_then(|id| panes.get(id));
        changed |= sb.set_focused_cwd(focused.and_then(|slot| slot.cwd.clone()));
        changed |= sb.set_last_exit(focused.and_then(|slot| slot.last_exit));
    }
    changed |= sidebar_painter.set_windows(windows);
    // phux-foz.9: the sidebar's agents section — one row per agent-running
    // pane, sourced from the ADR-0040 records with the OSC-title fallback.
    changed |= sidebar_painter.set_agents(agent_entries(workspace, panes, agent_meta));
    changed
}

/// Re-anchor the predictive-echo layer to a (newly) focused pane (phux-7ry0).
///
/// Predictions are pane-local, but the layer carries a single cursor anchor +
/// viewport. On a focus change it still holds the *previous* pane's bounds and
/// cursor; left stale, the first keystroke into the new pane echoes at the old
/// pane's coordinates — and because the pane-grid bounds clamp the next
/// outer-absolute resync, that lands mid-screen (the ghost echo after a split).
///
/// Reset the viewport to the new pane's grid and the cursor to its
/// authoritative pane-local position (or `(0, 0)` for a freshly spawned pane
/// that has not rendered yet), dropping any predictions anchored to the old
/// pane. Called from every focus-change site: click-to-focus, keybinding pane
/// navigation, and split (the new pane becomes focused).
pub(super) fn reanchor_predict_to_pane(
    predict: &mut PredictionState,
    panes: &HashMap<TerminalId, PaneSlot>,
    fid: &TerminalId,
) {
    let Some(slot) = panes.get(fid) else {
        // No slot yet — suspend until the pane's first snapshot syncs the
        // cursor and re-arms prediction.
        predict.suspend();
        return;
    };
    let cols = slot.terminal.cols().unwrap_or(0);
    let rows = slot.terminal.rows().unwrap_or(0);
    if cols > 0 && rows > 0 {
        // `set_viewport` drops the pending queue + the prompt-boundary anchor.
        predict.set_viewport(cols, rows);
    } else {
        predict.clear();
    }
    // Anchor on the pane's authoritative pane-local cursor; the overlay
    // re-adds the pane origin when painting. If the pane has not rendered yet
    // (a freshly split pane), there is NO real cursor — suspend rather than
    // anchor at (0, 0), or a quick keystroke echoes a ghost glyph at the
    // screen's top-left that the pane never overwrites (phux-7ry0 follow-up).
    match slot.renderer.last_cursor_local() {
        Some((row, col)) => predict.set_cursor(row, col),
        None => predict.suspend(),
    }
}

/// Window before a parser-pending bare ESC is interpreted as the Escape
/// key, anchored to when the ESC became pending (see `esc_deadline` in
/// `main_loop`). The client reads stdin from the *outer* terminal, which
/// writes a key's full `ESC [`/`ESC O` sequence in one burst — a split
/// only happens at a read-buffer boundary — so a short window suffices to
/// disambiguate. It must stay short: a modal-editor user pays this window
/// on EVERY bare Escape, and the inner application (vim's `ttimeoutlen`,
/// readline's `keyseq-timeout`) then stacks its own on top. tmux installs
/// ship `escape-time 0..10` for the same reason; 10ms keeps Escape under
/// the perception floor while still absorbing split sequences.
const ESC_FLUSH_IDLE: Duration = Duration::from_millis(10);

/// phux-jhv8: upper bound on how many already-queued frames one `recv`
/// wake-up drains before painting. A back-to-back output burst (nvim
/// startup) is a few dozen frames; the cap only guards against a server
/// that streams without pause starving the stdin/signal `select!` arms.
const FRAME_COALESCE_CAP: usize = 1024;

/// Safety valve for an application that enters DEC synchronized output and
/// never leaves it. Normal TUI transactions last milliseconds.
const SYNC_OUTPUT_WATCHDOG: Duration = Duration::from_secs(1);

/// The terminal a frame would repaint under normal handling, if any — the
/// `vt_write` + render pair a coalesced burst can defer to a later same-pane
/// frame (phux-jhv8). Output and snapshot frames carry pane content; every
/// other frame (layout, lifecycle, control) paints through its own path or
/// not at all, so it never defers (returns `None`).
const fn frame_paint_target(frame: &FrameKind) -> Option<&TerminalId> {
    match frame {
        FrameKind::TerminalOutput { terminal_id, .. }
        | FrameKind::TerminalSnapshot { terminal_id, .. } => Some(terminal_id),
        _ => None,
    }
}

/// Per-frame paint-deferral mask for a coalesced burst (phux-jhv8).
///
/// `targets[i]` is the pane frame `i` would repaint (`None` for control
/// frames). The result is `true` at `i` iff some later frame repaints the
/// *same* pane — meaning frame `i`'s paint is redundant and can be skipped
/// (its `vt_write` still applies). Each pane's LAST frame is therefore never
/// deferred, so every touched pane settles exactly once and none is left
/// stale; control frames (`None`) never defer.
fn coalesce_defer_flags(targets: &[Option<TerminalId>]) -> Vec<bool> {
    (0..targets.len())
        .map(|i| {
            targets[i].as_ref().is_some_and(|pane| {
                targets[i + 1..]
                    .iter()
                    .any(|later| later.as_ref() == Some(pane))
            })
        })
        .collect()
}

/// Whether a frame actually skips its paint in a coalesced burst.
///
/// `deferred_by_coalesce` is the per-pane last-wins mask from
/// [`coalesce_defer_flags`]. A frame defers iff that mask says so AND it is not
/// a [`FrameKind::TerminalSnapshot`]: a snapshot is authoritative full-screen
/// state and must always paint, never be superseded by a later same-pane
/// incremental `TerminalOutput` (whose partial paint assumes a screen the
/// snapshot never actually drew — the attach/reattach/split "mangled screen"
/// bug). Ordinary output frames still coalesce. The headless ingest path passes
/// `defer_paint = true` to `handle_server_frame` directly and never routes
/// through here, so its no-VT-emit invariant is unaffected.
const fn frame_defers_paint(deferred_by_coalesce: bool, frame: &FrameKind) -> bool {
    deferred_by_coalesce && !matches!(frame, FrameKind::TerminalSnapshot { .. })
}

/// Paint the active overlay layer (called only when an overlay is active).
///
/// Copy-mode is **not** a modal overlay: it repaints the focused pane with its
/// selection reverse-videoed — the live content is otherwise untouched, so
/// nothing on screen swaps — plus a status line. Every other overlay is modal:
/// clear the screen and paint its own surface. The branch is chosen by
/// [`OverlayState::copy_selection`].
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors paint_full_frame's paint context plus the overlay state"
)]
fn paint_active_overlay<W: super::RenderSink>(
    out: &mut W,
    overlays: &OverlayState,
    workspace: &Workspace,
    panes: &mut HashMap<TerminalId, PaneSlot>,
    focused: Option<&TerminalId>,
    // phux-x2hm: the driver's pane-zoom state. The base-frame repaints below
    // render through `Workspace::render_window` so the zoomed pane fills the
    // window; the copy-mode branch keeps using the REAL active window because
    // copy mode operates on the focused pane regardless of zoom.
    zoomed: Option<&TerminalId>,
    viewport_dims: (u16, u16),
    status_bar: Option<&mut StatusBarPainter>,
    // phux-4h5a: the sidebar reservation, so base-frame repaints under an
    // overlay keep panes inset (no reflow flicker when a modal opens).
    // `None` reservation (default) is byte-identical.
    sidebar: Option<SidebarReservation>,
    // phux-foz.10: the sidebar strip painter. The base-frame repaint under a
    // floating overlay starts with ED2 (full clear), so without the painter
    // the reserved columns stay blank and the sidebar vanishes for as long
    // as the palette / help / prompt / which-key overlay is open. Chrome
    // persists under overlays: overlays float above content, not above
    // chrome.
    sidebar_painter: Option<&mut crate::render::chrome::sidebar::SidebarPainter>,
    session_name: &str,
    theme: &crate::render::Theme,
) {
    // phux-foz.14: floating modals center inside the pane content rect (the
    // viewport minus the sidebar strip and status-bar row), NOT the raw
    // viewport, so a centered box never lands on the sidebar columns and
    // occludes the chrome the base-frame repaint (phux-foz.10) preserves. The
    // borrow of `status_bar` ends here (position is `Copy`), so it stays
    // available to move into `paint_full_frame` below.
    let overlay_content = {
        let bar_pos = status_bar.as_deref().map(StatusBarPainter::position);
        let cr = content_rect(viewport_dims, bar_pos, sidebar);
        ratatui::layout::Rect::new(cr.x, cr.y, cr.w, cr.h)
    };
    if let Some(sel) = overlays.copy_selection() {
        let (Some(ls), Some(fid)) = (workspace.active_window(), focused) else {
            return;
        };
        // Set the selection on the focused renderer for this one paint, repaint
        // the (zoom-honoring) base frame — the renderer inverts the selected
        // cells with their own styles — then clear it so ordinary renders are
        // unaffected. `ls` only gated the early-return on focus; the actual
        // paint goes through the zoomed view so the base matches the screen.
        let _ = ls;
        let base = workspace.render_window(zoomed);
        if let Some(slot) = panes.get_mut(fid) {
            slot.renderer.set_selection(Some(sel));
        }
        if let Some(base) = base.as_deref() {
            paint_full_frame(
                out,
                base,
                panes,
                focused,
                viewport_dims,
                status_bar,
                sidebar,
                sidebar_painter,
                session_name,
            );
        }
        if let Some(slot) = panes.get_mut(fid) {
            slot.renderer.set_selection(None);
        }
        let _ = paint_copy_mode_status(out, sel, viewport_dims, theme);
    } else if let Some(clip) = overlays.active_bounds(overlay_content) {
        // Floating modal (help / prompt / command palette / pickers): keep
        // the live panes visible by repainting the base frame, then emit
        // only the modal's bounded region on top. No `\x1b[2J` — the panes
        // surround the box instead of vanishing behind a full-screen clear.
        // The base frame includes the sidebar strip (phux-foz.10): the
        // repaint's own ED2 cleared it, and chrome must persist under a
        // floating overlay.
        if let Some(ls) = workspace.render_window(zoomed).as_deref() {
            paint_full_frame(
                out,
                ls,
                panes,
                focused,
                viewport_dims,
                status_bar,
                sidebar,
                sidebar_painter,
                session_name,
            );
        }
        let _ = overlays.paint_clipped(out, viewport_dims, overlay_content, clip, theme.shadow);
    } else {
        // Full-screen overlay (no bounded region): clear + paint.
        let _ = out.write_all(b"\x1b[2J\x1b[H");
        let _ = overlays.paint(out, viewport_dims);
    }
}

/// Emit the copy-mode status strip over the bottom viewport row, then hide the
/// hardware cursor (the reverse-video selection is the position indicator).
fn paint_copy_mode_status<W: Write>(
    out: &mut W,
    sel: SelectionRect,
    viewport_dims: (u16, u16),
    theme: &crate::render::Theme,
) -> io::Result<()> {
    let (cols, rows) = viewport_dims;
    if rows == 0 || cols == 0 {
        return Ok(());
    }
    let span_rows = u32::from(sel.end_row - sel.start_row + 1);
    let cell_count = if sel.rectangle {
        // Block (rectangle) selection: a columnar band on every spanned row, so
        // the count is span_rows * band_cols. The overlay only tuple-normalizes
        // the corners by `(row, col)`, which does NOT order the columns, so the
        // band width takes the min/max of the two column bounds — a plain
        // `end_col - start_col` would underflow whenever the drag runs up-left
        // (cursor column left of the anchor's on a lower row).
        let band_cols =
            u32::from(sel.start_col.max(sel.end_col) - sel.start_col.min(sel.end_col)) + 1;
        span_rows * band_cols
    } else {
        // Linear (text-flow) selection: the historical bounding-box arithmetic.
        // `saturating_sub` keeps the value identical for the ordered common
        // case while refusing to underflow on a multi-row drag whose corners
        // tuple-normalize to `start_col > end_col`.
        span_rows * (u32::from(sel.end_col.saturating_sub(sel.start_col)) + 1)
    };
    // Surface the active geometry from the one bit the renderer carries: block
    // (columnar band) vs linear (text-flow, incl. whole-line Line mode). `Tab`
    // cycles it (ADR-0045).
    let geom = if sel.rectangle { "block" } else { "linear" };
    let status = format!(
        " copy-mode | {geom} | {cell_count} cell(s) | arrows/PgUp/PgDn scroll | Tab mode | Enter copy | Esc "
    );
    write_cup(out, rows - 1, 0)?;
    // Selection strip from the theme (`selection_bg`/`selection_fg`). `\x1b[K`
    // fills the rest of the row with the strip bg; then reset + hide the cursor.
    out.write_all(b"\x1b[0m")?;
    crate::render::write_sgr_color(out, theme.selection_bg, false)?;
    crate::render::write_sgr_color(out, theme.selection_fg, true)?;
    let visible: String = status.chars().take(cols as usize).collect();
    out.write_all(visible.as_bytes())?;
    out.write_all(b"\x1b[K\x1b[0m\x1b[?25l")?;
    out.flush()
}

/// Errors the attach loop can surface to its caller.
///
/// Most variants wrap a richer underlying cause; the driver is careful to
/// fail fast rather than silently dropping protocol violations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AttachError {
    /// Local I/O error — UDS connect, socket read/write, stdin/stdout, or
    /// terminal ioctl.
    #[error("attach loop io error: {0}")]
    Io(#[source] io::Error),

    /// A remote transport could not be established: QUIC handshake, TLS
    /// certificate verification (a fingerprint that did not match the pin), or
    /// a refused/oversized auth preamble. Distinguished from local [`Self::Io`]
    /// so the CLI can point at the address, the pin, and the token rather than a
    /// missing socket file.
    #[error("transport connect error: {0}")]
    Connect(String),

    /// The remote host did not answer the dial: connection refused, no
    /// route, or handshake timeout. Distinguished from [`Self::Connect`]
    /// (which covers pin and auth failures on a host that answered) so the
    /// CLI can hint at overlay reachability instead of credentials.
    #[error("transport connect error: {0}")]
    Unreachable(String),

    /// The server closed the connection without sending `DETACHED`.
    /// Distinguished from a clean detach so the CLI can surface "server
    /// went away" vs "you detached".
    #[error("connection closed by server before DETACHED")]
    Disconnected,

    /// The server sent something we cannot interpret — undecodable frame,
    /// or a valid frame we don't expect at this point in the lifecycle.
    #[error("protocol error: {0}")]
    Protocol(String),

    /// Could not put the outer terminal into the expected state.
    #[error("terminal control error: {0}")]
    Terminal(String),

    /// Stdin is not a terminal. The attach loop needs a TTY because raw
    /// mode and alt-screen toggling require one. We bail early instead of
    /// silently no-op'ing.
    #[error("stdin is not a terminal; attach requires an interactive TTY")]
    NotATty,

    /// A libghostty operation failed on the client's local Terminal.
    #[error("libghostty: {0}")]
    Ghostty(#[from] libghostty_vt::Error),

    /// The server replied with a structured `ERROR` frame instead of
    /// `ATTACHED`. The session may not exist, the protocol version may
    /// have been rejected, or some other ATTACH-time server policy
    /// refused the request. The CLI surfaces this as actionable text.
    #[error("server refused attach: {0}")]
    Refused(String),
}

impl From<io::Error> for AttachError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<phux_dial::DialError> for AttachError {
    fn from(value: phux_dial::DialError) -> Self {
        match value {
            phux_dial::DialError::Io(err) => Self::Io(err),
            phux_dial::DialError::Connect(msg) => Self::Connect(msg),
            phux_dial::DialError::Unreachable(msg) => Self::Unreachable(msg),
        }
    }
}

impl From<super::render::RenderError> for AttachError {
    fn from(value: super::render::RenderError) -> Self {
        match value {
            super::render::RenderError::Io(e) => Self::Io(e),
            super::render::RenderError::Ghostty(e) => Self::Ghostty(e),
            super::render::RenderError::KittyReplay(e) => Self::Protocol(e.to_string()),
        }
    }
}

/// Public entry point: run an attach loop against `socket`, targeting
/// `target`. Blocks until the server sends `DETACHED` or the user
/// detaches.
///
/// The function is `async` because it relies on tokio; embedders must
/// drive it on a tokio runtime. Per ADR-0003 the canonical runtime is
/// `tokio::runtime::Builder::new_current_thread` — the returned future
/// is intentionally `!Send` because libghostty's `Terminal` is `!Send`
/// and lives on the attach task's stack across `await` points. The
/// single-threaded runtime never moves the future between threads.
///
/// # Ordering (`phux-roz`)
///
/// The expensive pre-handshake work — UDS connect, `HELLO`, `ATTACH`,
/// and the `ATTACHED` wait — runs on the *cooked* outer terminal.
/// Failures there propagate as `Err(_)` without ever entering raw mode
/// or the alt screen, so a missing server / bad session name / Ctrl-C
/// during connect prints a one-line error on the normal screen and
/// exits cleanly. Only after the server's `ATTACHED` frame arrives do
/// we flip the terminal into raw + alt screen via [`RawModeGuard`].
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub async fn run(socket: &Path, target: AttachTarget) -> Result<(), AttachError> {
    run_buffered(
        &Dial::uds(socket),
        target,
        PredictiveConfig::disabled(),
        None,
    )
    .await
}

/// Production attach: wrap stdout in the off-loop [`StdoutSink`](super::stdout_writer)
/// so a slow terminal never blocks the select loop (phux-fysb), then run the
/// session. Tests use the synchronous [`run_with_stdout`] seam directly.
///
/// `rec` is the `phux --rec` session recorder (ADR-0060). When present the
/// render sink is wrapped in a [`TeeSink`] *above* the `StdoutSink`, so the
/// recording sees every composited byte even in the moments the backlog cap
/// makes the glass drop frames.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
async fn run_buffered(
    dial: &Dial,
    target: AttachTarget,
    predict: PredictiveConfig,
    rec: Option<Rc<RefCell<SessionRecorder>>>,
) -> Result<(), AttachError> {
    let (mut sink, writer) = super::stdout_writer::spawn_stdout_writer();
    // Cloned BEFORE any wrap: the resync flag belongs to the StdoutSink, not
    // to whatever is layered on top of it.
    let resync = Arc::clone(&sink.needs_resync);
    if let Some(rec) = rec {
        let mut tee = TeeSink {
            inner: &mut sink,
            rec,
        };
        attach_session(
            dial,
            target,
            &mut tee,
            predict,
            Some(resync.as_ref()),
            Some(writer),
            true,
        )
        .await
    } else {
        attach_session(
            dial,
            target,
            &mut sink,
            predict,
            Some(resync.as_ref()),
            Some(writer),
            true,
        )
        .await
    }
}

/// Like [`run`], with predictive local echo configurable per call.
///
/// `predict.enabled = false` is identical to [`run`]; `predict.enabled =
/// true` engages the Mosh-class prediction layer documented in
/// [`crate::predict`] (`phux-9gw.1`).
///
/// Kept as a separate entry point (rather than a new parameter on
/// [`run`]) so existing callers — and the integration tests in
/// `phux-server` that exercise the attach loop — continue to compile
/// without churn.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub async fn run_with_predict(
    socket: &Path,
    target: AttachTarget,
    predict: PredictiveConfig,
) -> Result<(), AttachError> {
    run_buffered(&Dial::uds(socket), target, predict, None).await
}

/// Dial-aware production attach (UDS *or* QUIC) with predictive echo config.
///
/// The transport-agnostic sibling of [`run_with_predict`]: the CLI builds a
/// [`Dial`] from its flags (a UDS path or a remote `--quic` target) and the
/// same off-loop-stdout production path runs regardless of byte plumbing.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub async fn run_with_predict_dial(
    dial: &Dial,
    target: AttachTarget,
    predict: PredictiveConfig,
) -> Result<(), AttachError> {
    run_buffered(dial, target, predict, None).await
}

/// As [`run_with_predict_dial`], but tees the composited output stream into
/// `rec` — the `phux --rec` session recording (ADR-0060).
///
/// The recorder is passed in (rather than opened here) because the CLI must
/// keep the SAME recording across a graceful-upgrade reconnect: the attach
/// loop can return and be re-entered, and a recorder created per attempt
/// would truncate the cast at every reconnect.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub async fn run_recorded_dial(
    dial: &Dial,
    target: AttachTarget,
    predict: PredictiveConfig,
    rec: Rc<RefCell<SessionRecorder>>,
) -> Result<(), AttachError> {
    run_buffered(dial, target, predict, Some(rec)).await
}

/// Same as [`run`], but writes the entire composited output stream to a
/// caller-supplied [`RenderSink`](super::RenderSink) (any `Write`).
///
/// The stream covers alt-screen enter, cursor hide, every pane's per-row
/// CUP/SGR, the status bar, overlays, and cleanup.
///
/// The renderer and all chrome painters are generic over `Write`, and the
/// driver threads this one sink through `main_loop` into
/// `handle_server_frame`, `paint_full_frame`, and `dispatch_input_events`.
/// So the whole attach render path is injectable: production passes real
/// stdout via [`run`]; tests and the headless agent surface pass a
/// `Vec<u8>` (or any other `Write`) and read back the captured VT.
///
/// Exposed so tests can capture the byte stream and assert on it — in
/// particular, the regression guard for `phux-roz` asserts that the
/// pre-handshake failure path NEVER emits `\x1b[?1049h`. The stdin /
/// signal / termios cleanup paths run on real stdout regardless of the
/// injected sink (Drop / signal handlers can't reach it).
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub async fn run_with_stdout<W: super::RenderSink>(
    socket: &Path,
    target: AttachTarget,
    out: &mut W,
) -> Result<(), AttachError> {
    run_with_stdout_predict(socket, target, out, PredictiveConfig::disabled()).await
}

/// As [`run_with_stdout`], but with an explicit predictive-echo config.
/// Production callers should reach for [`run_with_predict`]; this is the
/// test-injectable variant.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub async fn run_with_stdout_predict<W: super::RenderSink>(
    socket: &Path,
    target: AttachTarget,
    out: &mut W,
    predict: PredictiveConfig,
) -> Result<(), AttachError> {
    // Synchronous-sink test seam: no off-loop writer, no resync flag.
    attach_session(&Dial::uds(socket), target, out, predict, None, None, false).await
}

/// Headless one-shot: attach, ingest the session's snapshot + layout, and
/// return the client's composited multi-pane view as dense structured cells
/// (`phux snapshot --rendered`, phux-l5xa).
///
/// Unlike the side-effect-free `GET_SCREEN` read, this **attaches** (R2): it
/// drives the same client render path the live attach loop uses, so the
/// returned frame is what the human's glass would show — pane content tiled
/// per the layout, dividers, and the status bar, composited. But it never
/// installs raw mode or an alt screen and never paints VT: frames feed the
/// pane mirrors with `defer_paint = true` (mirrors ingest, stdout is
/// suppressed), then ONE `rendered::compose_full_frame_cells` pass
/// assembles the frame. There is no TTY, so the viewport `(cols, rows)` is
/// caller-supplied.
///
/// Settle policy (R3): after the ATTACHED replay and the layout `GET`,
/// frames are drained until the stream goes idle for `SETTLE_IDLE` (the
/// server's initial snapshot burst has landed) or an overall
/// `SETTLE_DEADLINE` elapses — a quiescence wait on real frame arrival, not
/// a fixed sleep.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
#[allow(
    clippy::too_many_lines,
    reason = "mirrors main_loop's session-scoped local setup before one ingest-and-compose; the ~12 &mut locals would otherwise be threaded through a context struct for a single caller"
)]
pub async fn run_headless_rendered(
    socket: &Path,
    target: AttachTarget,
    cols: u16,
    rows: u16,
) -> Result<phux_core::screen::RenderedFrame, AttachError> {
    use std::time::SystemTime;

    /// Idle gap that marks the server's initial snapshot burst complete.
    const SETTLE_IDLE: Duration = Duration::from_millis(120);
    /// Hard cap on the whole drain, guarding a pathological never-idle stream.
    const SETTLE_DEADLINE: Duration = Duration::from_secs(3);

    let mut conn = Connection::connect(socket).await?;
    let _mode = handshake(&mut conn, None).await?;
    send_attach(&mut conn, target).await?;
    let attached = wait_for_attached(&mut conn).await?;

    let viewport_dims = (cols.max(1), rows.max(1));
    // Throwaway sink: `defer_paint = true` emits no VT, but
    // `handle_server_frame` still needs a `Write`.
    let mut sink: Vec<u8> = Vec::new();
    let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
    let mut workspace = Workspace::default();
    let mut focused_pane: Option<TerminalId> = None;
    let mut zoomed: Option<TerminalId> = None;
    let mut session_name = String::new();
    let mut status_bar = build_status_bar_painter();
    // phux-4h5a: read `[sidebar]` so `phux snapshot --rendered` shows the
    // strip exactly as a live attach would. Disabled (the default) folds to
    // `None`, keeping the rendered frame byte-identical to the pre-sidebar one.
    let headless_cfg = phux_config::loader::load().ok();
    let sidebar_cfg = headless_cfg.as_ref().map(|c| c.sidebar.clone());
    let sidebar = sidebar_cfg
        .as_ref()
        .filter(|c| c.enabled)
        .map(|c| SidebarReservation {
            edge: match c.position {
                SidebarPosition::Right => SidebarEdge::Right,
                SidebarPosition::Left => SidebarEdge::Left,
            },
            width: c.width,
        });
    let sidebar_theme = headless_cfg
        .as_ref()
        .map_or_else(crate::render::Theme::default, |c| {
            crate::render::Theme::from_cfg(&c.theme)
        });
    let mut predict = PredictionState::new(
        PredictiveConfig::disabled(),
        viewport_dims.0,
        viewport_dims.1,
    );
    let overlay = Overlay;
    let mut pending_splits: HashMap<u32, PendingSplit> = HashMap::new();
    let mut pending_windows: HashMap<u32, PendingWindow> = HashMap::new();
    let mut layout_get_request_id: Option<u32> = None;
    // ADR-0040: one-shot `phux.agent/v1` reads so the composited window
    // labels prefer structured agent records, matching a live attach.
    let mut agent_meta = AgentMetaIndex::default();
    // phux-p4vp: pane cwd + branch memo so the composited sidebar carries
    // the same branch lines a live attach would.
    let mut vcs = VcsIndex::default();

    // Replay ATTACHED so the focused-pane + workspace bootstrap runs once.
    let outcome = handle_server_frame(
        &mut sink,
        attached,
        &mut panes,
        &mut workspace,
        &mut focused_pane,
        &mut zoomed,
        &mut session_name,
        status_bar.as_mut(),
        sidebar,
        viewport_dims,
        &mut predict,
        &overlay,
        layout_get_request_id,
        &mut pending_splits,
        &mut pending_windows,
        &mut agent_meta,
        false,
        true,
    )?;
    vcs.apply_snapshot(outcome.pane_cwds);
    let focused_session = outcome.sessions.map(|(_, focused)| focused);

    // ADR-0040: pipeline one `phux.agent/v1` GET per pane (no SUBSCRIBE —
    // this is a one-shot composite). Replies drain through the settle loop
    // below and land in `agent_meta.records`. Request ids start high above
    // the layout GET's `1` so the two reply streams cannot collide.
    {
        let mut req_id: u32 = 1000;
        for id in panes.keys() {
            agent_meta.pending.insert(req_id, id.clone());
            conn.send(&FrameKind::GetMetadata {
                request_id: req_id,
                scope: Scope::Terminal(id.clone()),
                key: TERMINAL_AGENT_KEY.to_owned(),
            })
            .await?;
            req_id = req_id.wrapping_add(1);
        }
    }

    // Pull any persisted multi-pane layout for this session so dividers +
    // tiling match a live attach. One-shot, so we GET but do not SUBSCRIBE.
    if outcome.subscribe_layout
        && let Some(session) = focused_session
    {
        let req_id = 1;
        layout_get_request_id = Some(req_id);
        conn.send(&FrameKind::GetMetadata {
            request_id: req_id,
            scope: Scope::Group(DEFAULT_GROUP_ID),
            key: layout_key(session),
        })
        .await?;
    }

    // Drain the initial burst until the stream goes idle (or the deadline).
    let _ = tokio::time::timeout(SETTLE_DEADLINE, async {
        loop {
            tokio::select! {
                biased;
                frame = conn.recv() => {
                    let frame = frame?;
                    handle_server_frame(
                        &mut sink,
                        frame,
                        &mut panes,
                        &mut workspace,
                        &mut focused_pane,
                        &mut zoomed,
                        &mut session_name,
                        status_bar.as_mut(),
                        sidebar,
                        viewport_dims,
                        &mut predict,
                        &overlay,
                        layout_get_request_id,
                        &mut pending_splits,
                        &mut pending_windows,
                        &mut agent_meta,
                        false,
                        true,
                    )?;
                }
                () = tokio::time::sleep(SETTLE_IDLE) => break,
            }
        }
        Ok::<(), AttachError>(())
    })
    .await;

    // Seed the window/tab strip exactly as the live loop does before its
    // first bar paint, so the composited bar shows the windows.
    let windows = window_infos(
        &workspace,
        &panes,
        zoomed.as_ref(),
        &agent_meta.records,
        &mut vcs,
    );
    if let Some(sb) = status_bar.as_mut() {
        sb.set_windows(windows.clone());
    }
    // phux-4h5a: feed the same window list into the strip painter so the
    // composited frame shows the sidebar tabs when `[sidebar]` is enabled.
    let mut sidebar_painter = SidebarPainter::new(sidebar_theme);
    sidebar_painter.set_windows(windows);
    // phux-foz.9: and the agents section, from the same record index +
    // title fallback a live attach renders.
    sidebar_painter.set_agents(agent_entries(&workspace, &panes, &agent_meta));

    // Compose the assembled frame against the render layout (honoring zoom).
    let layout_state = workspace.render_window(zoomed.as_ref()).map_or_else(
        crate::layout::LayoutState::default,
        std::borrow::Cow::into_owned,
    );
    let frame = super::rendered::compose_full_frame_cells(
        &layout_state,
        &mut panes,
        focused_pane.as_ref(),
        viewport_dims,
        status_bar.as_ref(),
        sidebar,
        Some(&sidebar_painter),
        &session_name,
        SystemTime::now(),
    );
    Ok(frame)
}

/// The attach session body shared by the production ([`run`]) and
/// test-injectable ([`run_with_stdout_predict`]) entry points.
///
/// `resync` is the [`StdoutSink`](super::stdout_writer) backpressure flag
/// (`None` for the synchronous test sink); `main_loop` polls it to repaint
/// the latest state after the writer dropped a stale backlog. `writer` is the
/// off-loop stdout writer's handle (`None` for the test sink); it is drained
/// and joined before the terminal-reset writes on every exit path so output
/// isn't lost and the reset isn't garbled.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
async fn attach_session<W: super::RenderSink>(
    dial: &Dial,
    target: AttachTarget,
    out: &mut W,
    predict: PredictiveConfig,
    resync: Option<&AtomicBool>,
    mut writer: Option<super::stdout_writer::WriterHandle>,
    probe_default_colors: bool,
) -> Result<(), AttachError> {
    // STAGE 1 — pre-handshake, on the cooked outer terminal.
    //
    // We deliberately do NOT install RawModeGuard here. If anything in
    // this block fails (no server, refused, signal during connect) the
    // user's terminal stays in its original state and `Err(_)` carries
    // the actionable cause up to the CLI.
    let mut conn = Connection::connect_dial(dial).await?;
    // Attach-handshake timing (info): HELLO -> ATTACH -> ATTACHED. The
    // span's CLOSE duration is the end-to-end attach latency a trace reader
    // wants for "why was the first paint slow." Lifecycle-rate, so info.
    let handshake_span = tracing::info_span!("attach_handshake", ?target);
    let (attached, output_mode) = async {
        let default_colors = probe_default_colors
            .then(super::terminal_probe::default_colors)
            .flatten();
        let mode = handshake(&mut conn, default_colors).await?;
        send_attach(&mut conn, target).await?;
        let attached = wait_for_attached(&mut conn).await?;
        Ok::<_, AttachError>((attached, mode))
    }
    .instrument(handshake_span)
    .await?;
    // The output mode is a per-connection HELLO property; `handshake`
    // runs exactly once per connection and the re-attach loop below reuses
    // the same `conn` without re-running it, so this bool is stable across
    // an in-connection session switch. Only a `StateSync` consumer's
    // `FRAME_ACK`s feed the server's per-seq RTT/backpressure accounting;
    // a raw consumer's acks are dropped server-side, so the loop skips them.
    let wants_state_sync = output_mode == OutputMode::StateSync;

    // STAGE 2 — server accepted the attach. Now and only now do we flip
    // the outer terminal into raw + alt screen. The guard's Drop runs
    // on unwinding; the signal-handler path inside `main_loop` runs
    // `write_terminal_reset` explicitly to cover SIGINT/SIGTERM/SIGHUP.
    //
    // ADR-0048: read the `mouse` config (default on) to decide whether the
    // guard also enables the client's own outer-terminal mouse tracking, so
    // divider drag-to-resize works by default. A load failure or an
    // explicit `mouse = false` falls back to pass-through-only — no DECSET,
    // host native selection untouched.
    let mouse_capture = phux_config::loader::load()
        .map(|c| c.defaults.mouse)
        .unwrap_or(true);
    let _guard = RawModeGuard::install_with_stdout(out, mouse_capture)?;

    // Install a panic hook so an unexpected panic inside `main_loop`
    // (renderer bug, libghostty FFI surprise, etc.) still restores the
    // terminal before the default hook prints its backtrace. The hook
    // is global, so we only register it once per process.
    install_panic_hook_once();

    // phux-eb0: outer re-attach loop. `main_loop` is single-session by
    // construction (it builds ~15 session-scoped locals and replays the
    // ATTACHED frame once on entry). When the user picks another session
    // via `<leader> a` the loop returns `LoopExit::SwitchTo(name)`; here
    // we detach from the current session, re-run the ATTACH handshake
    // against `ByName(name)` on the SAME transport connection (a session
    // switch is within one server, so the UDS connection — bound to the
    // server, not to any single session — is reused, not reconnected),
    // and re-enter `main_loop` with the new ATTACHED frame. The
    // `RawModeGuard` stays installed across the switch (it lives in this
    // outer scope) so the alt screen never flickers and the terminal is
    // never left in a bad state. On `Detached` the loop exits via
    // `exit_after_detach` (which never returns — see its doc comment).
    let mut attached = attached;
    // phux-foz.6: first-run onboarding hint, decided ONCE per attach
    // invocation by a single existence check at the canonical config path
    // (see `super::onboarding` for the trigger rule). `mem::take` hands the
    // flag to the first `main_loop` entry only, so an in-invocation session
    // switch (which re-enters `main_loop` with fresh session state) does
    // not re-show a hint the user already dismissed.
    let mut show_onboarding = super::onboarding::should_show(&phux_config::loader::config_path());
    // phux-foz.8: window index to select after a one-step cross-session
    // window pick (`switch-session { name, window }`) re-attaches. `None`
    // on the first attach and after plain switches; set per-iteration by
    // the SwitchTo arm below, consumed by `main_loop` once the target's
    // persisted layout loads. phux-jpqd: `pending_pane` is the pane half
    // of a one-step cross-session pane pick (`switch-session { .., pane }`).
    let mut pending_window: Option<usize> = None;
    let mut pending_pane: Option<usize> = None;
    loop {
        let show_onboarding_hint = std::mem::take(&mut show_onboarding);
        let exit = match main_loop(
            &mut conn,
            attached,
            predict,
            out,
            resync,
            wants_state_sync,
            show_onboarding_hint,
            pending_window.take(),
            pending_pane.take(),
        )
        .await
        {
            Ok(exit) => exit,
            Err(err) => {
                // Drain + stop the off-loop writer before propagating; the
                // RawModeGuard's Drop restores the terminal as we unwind.
                if let Some(writer) = writer.take() {
                    writer.shutdown_and_join();
                }
                return Err(err);
            }
        };
        match exit {
            LoopExit::Detached => {
                // Lifecycle transition (info): the attach loop is exiting.
                tracing::info!("attach loop: DETACHED; exiting");
                // The session ended (user detach, server `DETACHED`, or a
                // detach-intended disconnect). Restore the terminal and
                // exit now rather than returning up the stack: a returning
                // `Ok(())` would let the tokio runtime drop block forever
                // on the uncancellable stdin read thread (see
                // `exit_after_detach`'s doc comment).
                //
                // Drain queued output + stop the writer FIRST so nothing is
                // lost and the reset writes in `exit_after_detach` aren't
                // garbled by an in-flight frame.
                if let Some(writer) = writer.take() {
                    writer.shutdown_and_join();
                }
                exit_after_detach();
            }
            LoopExit::SwitchTo(target) => {
                // Lifecycle transition (info): switching sessions on the
                // same connection. `?target` names the destination.
                tracing::info!(?target, "attach loop: SWITCH_TO; re-attaching");
                // Tear down the current session on the server so it frees
                // our per-consumer reference grid + reaps the detached
                // consumer (the just-landed per-consumer detach reaping —
                // don't leak). DETACH does NOT close the connection
                // server-side (see `phux-server::runtime`'s DETACH arm:
                // it emits DETACHED and keeps the read loop alive), so the
                // same `conn` is reusable for the new ATTACH.
                detach_and_drain(&mut conn).await?;
                // Re-run the handshake against the new target on the same
                // connection. No reconnect: one server owns all the
                // sessions, and the transport is bound to the server, not
                // to any single session. An existing session re-attaches
                // by name; a new-session request creates it (or attaches if
                // the name is already taken) via CreateIfMissing.
                // phux-foz.8: a one-step window pick carries the target
                // window; stash it for the next `main_loop` entry, which
                // resolves it once the new session's layout loads. phux-jpqd:
                // a foreign fleet row also carries the target pane, resolved
                // after the window select.
                let attach_target = match target {
                    ReattachTarget::Existing { name, window, pane } => {
                        pending_window = window;
                        pending_pane = pane;
                        AttachTarget::ByName(name)
                    }
                    ReattachTarget::Create(name) => create_session_target(name),
                };
                send_attach(&mut conn, attach_target).await?;
                attached = wait_for_attached(&mut conn).await?;
                tracing::info!("attach loop: re-attach handshake complete");
                // Re-enter `main_loop`, which rebuilds ALL session-scoped
                // state fresh (pane mirrors, workspace, predict, overlays,
                // pending-spawn maps, layout subscription) from the new
                // ATTACHED frame, then repaints. A full repaint of the new
                // session's grid happens via the replayed ATTACHED +
                // TERMINAL_SNAPSHOT frames inside the loop.
                let _ = write_terminal_clear(out);
            }
        }
    }
}

/// Build the `CreateIfMissing` target for an in-TUI session create (the
/// session picker's "new session" row, phux-0db).
///
/// Carries the client process's current working directory instead of
/// `cwd: None`: `None` on the wire seeds the pane in the *daemon's* CWD
/// (typically `$HOME` for a long-lived server), which breaks tools whose
/// persistence is keyed by directory (e.g. `claude --resume`). The server
/// validates the path and falls back to its default spawn directory when
/// it is not an enterable directory on the server host, so a stale or
/// remote-client path can never fail the create.
fn create_session_target(name: String) -> AttachTarget {
    AttachTarget::CreateIfMissing {
        name,
        command: None,
        cwd: std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned()),
    }
}

/// phux-eb0: send `DETACH` and drain frames until `DETACHED` arrives, so
/// the server-side per-consumer state (reference grid, subscriber lists)
/// is released before the next `ATTACH` on the same connection.
///
/// Frames that arrive between our `DETACH` and the server's `DETACHED`
/// (a `TERMINAL_OUTPUT` already in flight, a late `METADATA_CHANGED`) are
/// discarded — we are tearing the session down and rebuilding all
/// session-scoped state on the next attach, so nothing in this window is
/// worth applying. A server-initiated disconnect during the drain is a
/// genuine error (the switch can't complete), surfaced as
/// `AttachError::Disconnected`.
async fn detach_and_drain(conn: &mut Connection) -> Result<(), AttachError> {
    conn.send(&FrameKind::Detach).await?;
    loop {
        match conn.recv().await? {
            FrameKind::Detached => return Ok(()),
            other => {
                tracing::trace!(kind = ?other, "draining frame during session switch");
            }
        }
    }
}

/// phux-eb0: clear the alt screen between sessions so the previous
/// session's grid doesn't briefly show under the new session's first
/// paint. The new `ATTACHED` + `TERMINAL_SNAPSHOT` repaint lands
/// immediately after, so this is a one-frame clear, not a flicker.
fn write_terminal_clear<W: Write>(out: &mut W) -> io::Result<()> {
    out.write_all(b"\x1b[2J\x1b[H")?;
    out.flush()
}

/// Whether to emit a `FRAME_ACK` for an applied `TERMINAL_OUTPUT`.
///
/// Acks are load-bearing only for a `StateSync` consumer: the server
/// folds each into that consumer's per-seq RTT/backpressure accounting
/// (`on_frame_ack`). A raw broadcast consumer's acks carry no seq the
/// server tracks, so it drops them — emitting one is a wasted client
/// write plus a server decode and state lock on the same UDS that carries
/// keystrokes during a repaint storm. In raw mode the ack is skipped; in
/// state-sync mode the `(terminal_id, seq)` flows through unchanged.
///
/// Not `const`: the `(TerminalId, u64)` it threads carries a non-trivial
/// destructor (the federation `TerminalId::Satellite` variant owns a
/// `String`), which a `const fn` may not drop at compile time.
fn should_emit_frame_ack(
    wants_state_sync: bool,
    ack: Option<(TerminalId, u64)>,
) -> Option<(TerminalId, u64)> {
    if wants_state_sync { ack } else { None }
}

/// Send `HELLO` and require `HELLO_OK` before ATTACH. Returns the
/// [`OutputMode`] the client advertised — a per-connection HELLO
/// property the caller threads into the session loop to decide whether
/// `FRAME_ACK` accounting is load-bearing.
async fn handshake(
    conn: &mut Connection,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) -> Result<OutputMode, AttachError> {
    // Sniff `$COLORTERM` / `$TERM` / `$TERM_PROGRAM` per
    // `detect_color_support`. The advertised tier feeds the server's
    // per-client `downsample::rewrite_bytes` (SPEC §6.2).
    //
    // phux-4li.5: declare L3 (`Layer::L3`) so the server forwards
    // `MetadataChanged` events for the `phux.tui.layout/v1` key — the
    // reconcile-on-attach path in `main_loop` subscribes to that key
    // and re-renders multi-pane when another client mutates the layout.
    let mut client_caps = ClientCapabilities::new()
        .with_color_support(detect_color_support())
        .with_layers(LayerSet::with(&[Layer::L3]));
    if let Some(colors) = default_colors {
        client_caps = client_caps.with_default_colors(colors);
    }
    conn.send(&FrameKind::Hello {
        client_name: format!("phux-client/{}", env!("CARGO_PKG_VERSION")),
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        client_caps,
    })
    .await?;
    match conn.recv().await? {
        FrameKind::HelloOk { .. } => Ok(client_caps.output_mode),
        FrameKind::Error { message, .. } => Err(AttachError::Refused(message)),
        other => Err(AttachError::Protocol(format!(
            "expected HELLO_OK or ERROR after HELLO, got {other:?}",
        ))),
    }
}

/// Send the `ATTACH` frame using the current terminal viewport.
async fn send_attach(conn: &mut Connection, target: AttachTarget) -> Result<(), AttachError> {
    let viewport = current_viewport()?;
    conn.send(&FrameKind::Attach {
        target,
        viewport,
        // SPEC §13: clients SHOULD opt in to scrollback. The cap below
        // matches the default in docs/consumers/tui.md §X; a configurable knob lives
        // with the rest of `phux-config`.
        request_scrollback: true,
        scrollback_limit_lines: 10_000,
    })
    .await
}

/// Read frames off `conn` until we get the expected `ATTACHED` reply,
/// surfacing a structured `Error` frame as `AttachError::Refused` and
/// any other unexpected frame as `AttachError::Protocol`.
///
/// Runs entirely on the cooked terminal (pre-`RawModeGuard`) per
/// `phux-roz`. A server-side reject prints an actionable error on the
/// normal screen and exits without flicker.
async fn wait_for_attached(conn: &mut Connection) -> Result<FrameKind, AttachError> {
    let frame = conn.recv().await?;
    match frame {
        FrameKind::Attached { .. } => Ok(frame),
        FrameKind::Error {
            code: _, message, ..
        } => Err(AttachError::Refused(message)),
        other => {
            // Anything else this early is a protocol violation. The
            // server is required to answer `ATTACH` with either
            // `ATTACHED` or `ERROR`; reject otherwise rather than
            // silently soldiering on into a half-attached state.
            Err(AttachError::Protocol(format!(
                "expected ATTACHED or ERROR after ATTACH, got {other:?}",
            )))
        }
    }
}

/// phux-eb0: how the `main_loop` `select!` loop terminated.
///
/// `main_loop` is single-session by construction — it builds all the
/// session-scoped locals up front and replays one ATTACHED frame. Rather
/// than tear down and rebuild that state in place, the loop signals its
/// caller ([`run_with_stdout_predict`]'s outer loop) which way it exited:
///
/// * `Detached` — the user detached or the server sent `DETACHED`. The
///   loop already ran `exit_after_detach` on that path (which never
///   returns); the outer loop treats a returned `Detached` as a clean
///   exit too.
/// * `SwitchTo(target)` — the user committed `switch-session { name }`
///   (via the `<leader> a` picker / palette) or `new-session`. The outer
///   loop detaches from the current session and re-runs the handshake
///   against the target (`ByName` for an existing session, `CreateIfMissing`
///   for a new one), then re-enters `main_loop` with the new ATTACHED frame
///   and freshly-rebuilt session state.
#[derive(Debug)]
enum LoopExit {
    /// The session ended (detach / server DETACHED). The process exits.
    Detached,
    /// Re-attach on the same connection — to an existing session or a
    /// newly-created one.
    SwitchTo(ReattachTarget),
}

/// Drive the `tokio::select!` loop until detach or a session switch.
///
/// `initial_attached` is the `FrameKind::Attached` frame that
/// [`wait_for_attached`] already pulled off the wire; we replay it
/// through `handle_server_frame` so the focused-pane bookkeeping lives
/// in one place. Subsequent `TERMINAL_SNAPSHOT` / `TERMINAL_OUTPUT` frames come
/// off the wire as usual.
///
/// phux-eb0: returns a [`LoopExit`] so the outer loop in
/// [`run_with_stdout_predict`] can re-attach to another session without
/// dropping the transport or leaving raw mode. Every session-scoped local
/// in this function is rebuilt on each entry, so a re-attach starts from a
/// clean slate (no stale pane mirror, no carried-over predict queue).
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
#[allow(
    clippy::too_many_lines,
    reason = "tokio::select! arms inflate function length; splitting would require carrying ~10 mutable locals through helpers"
)]
#[allow(
    clippy::cognitive_complexity,
    reason = "select! arms + phux-4li.5 outcome dispatch; ditto"
)]
#[allow(
    clippy::too_many_arguments,
    reason = "per-entry knobs from attach_session's outer loop; foz-6 onboarding + foz-8 window pick + jpqd cross-session pane pick"
)]
async fn main_loop<W: super::RenderSink>(
    conn: &mut Connection,
    initial_attached: FrameKind,
    predict_cfg: PredictiveConfig,
    out: &mut W,
    // phux-fysb: the off-loop StdoutSink's backpressure flag. When the writer
    // drops a stale backlog under a slow terminal it sets this; we repaint the
    // latest state from scratch (a self-contained full frame supersedes the
    // dropped diffs). `None` for the synchronous test sink.
    needs_resync: Option<&AtomicBool>,
    // Whether this connection negotiated `OutputMode::StateSync`. Gates the
    // per-frame `FRAME_ACK`: only a state-sync consumer's acks are tracked
    // server-side, so a raw consumer skips them (see `should_emit_frame_ack`).
    wants_state_sync: bool,
    // phux-foz.6: show the first-run onboarding hint after the bootstrap
    // paint. The caller decides this once per attach invocation (config-path
    // existence check in `attach_session`'s outer loop) and passes `true`
    // only on the first `main_loop` entry, so a session switch never
    // re-shows a hint the user already dismissed.
    show_onboarding: bool,
    // phux-foz.8: window index to select once this session's persisted
    // layout loads. Set by the outer loop when a one-step cross-session
    // window pick (`switch-session { name, window }`) drove the re-attach;
    // `None` on a plain attach/switch. Resolved (and consumed) on the
    // first layout reconcile; out-of-range degrades to the session's own
    // restored focus with a warning.
    initial_window: Option<usize>,
    // phux-jpqd: DFS leaf ordinal to focus (within `initial_window`) once
    // this session's layout loads — the pane half of a one-step
    // cross-session PANE pick (`switch-session { name, window, pane }`,
    // the agent-fleet foreign rows). `None` on a plain switch or a
    // window-only pick; resolved alongside `initial_window` and, like it,
    // degrades to a logged no-op if out of range.
    initial_pane: Option<usize>,
) -> Result<LoopExit, AttachError> {
    // phux-4li.4: hold N client-side Terminals keyed by `TerminalId`,
    // not the single Terminal of the wave-A driver. Each pane's slot is
    // allocated lazily — the first `TERMINAL_SNAPSHOT` or
    // `TERMINAL_OUTPUT` carrying a given id seeds it via
    // `panes.entry(id).or_insert_with(PaneSlot::new)`. The
    // `Workspace` mirror (initialized as a single window holding one
    // pane when `ATTACHED` lands; see `handle_server_frame`) is the
    // source of truth for which leaves are live and where they sit in
    // the outer viewport. The renderer and layout helpers operate on the
    // active window (`workspace.active_window()`); the workspace
    // dimension is what gets persisted to L3.
    let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
    let mut workspace = Workspace::default();
    let mut focused_pane: Option<TerminalId> = None;
    // phux-oih5.4: one-entry focus MRU, local to this attached client. It is
    // deliberately outside Workspace so layout metadata never persists or
    // shares focus history (ADR-0019 decision 6).
    let mut focus_history = super::focus::FocusHistory::default();
    // ADR-0033: this client's own server-assigned ClientId, captured from
    // ATTACHED. Used to render "you hold the wheel" vs another client in the
    // supervisory badge. `None` until ATTACHED lands.
    let mut own_client_id: Option<ClientId> = None;
    // phux-x2hm: pane-zoom view state (driver-local, like focus). `Some(id)`
    // ⇒ pane `id` is zoomed to fill the window; render/reflow then run against
    // `workspace.render_window(zoomed)` (a synthetic single-leaf layout)
    // instead of the real tiled tree, which is left untouched for mutation.
    let mut zoomed: Option<TerminalId> = None;
    // phux-4li.5: request-id allocator for L3 GET correlation. (The
    // keybind resolver is built below, from the plugin-merged
    // keybindings snapshot.)
    let mut layout_get_request_id: Option<u32> = None;
    let mut next_request_id: u32 = 1;
    // phux-4li.12: in-flight `split-pane` actions parked by request id.
    // Populated by `run_action` when it dispatches SPAWN_TERMINAL;
    // drained by `handle_server_frame`'s TerminalSpawned arm when the
    // reply arrives. The map is small (one entry per outstanding
    // user-triggered split) so a HashMap is overkill for cap but
    // matches the layout-key request-id pattern.
    let mut pending_splits: HashMap<u32, PendingSplit> = HashMap::new();
    // phux-4li.15: in-flight `new-window` actions parked by request id,
    // same lifecycle as `pending_splits`. The TerminalSpawned arm checks
    // this map first; a hit opens a new window on the spawned pane.
    let mut pending_windows: HashMap<u32, PendingWindow> = HashMap::new();
    // ADR-0040 (phux-3ert): the structured agent-identity index. Each pane
    // gets a one-shot `GET_METADATA` + a live `SUBSCRIBE_METADATA` on
    // `phux.agent/v1` (see `sync_agent_meta_subscriptions`); decoded records
    // feed the window labels so the sidebar/tab strip renders agent
    // name/state from structured data, with the OSC title as the fallback.
    let mut agent_meta = AgentMetaIndex::default();
    // phux-p4vp: pane cwd + branch memo behind the sidebar's branch line.
    // Seeded from every ATTACHED snapshot; read at chrome-refresh time.
    let mut vcs = VcsIndex::default();
    // phux-nz4.5: status-bar painter, built from the on-disk config.
    // Load failures fall back to an empty bar so a malformed config
    // never blocks attach — the user still gets a working pane mirror.
    let mut status_bar = build_status_bar_painter();
    // phux-5ke.4: keybindings snapshot for the help overlay. Cached so
    // pressing the help binding doesn't trigger a synchronous config
    // reload (which could surface IO errors under user fingers); on
    // load failure the help modal still works, just showing "no
    // bindings configured".
    // phux-ahv.4: load the config once and split out both the
    // keybindings snapshot (help overlay) and the color theme (chrome +
    // overlays). On load failure both fall back to defaults — the help
    // modal shows "no bindings" and chrome paints with the built-in
    // palette.
    let loaded_cfg = phux_config::loader::load().ok();
    // phux-r82.5 / phux-r82.7: snapshot the enabled plugins' manifests once
    // at driver start (same policy as the keybindings snapshot — no config
    // I/O under user fingers), then derive both the action entries (palette
    // rows + manifest `keys` merged into the prefix table below, user config
    // winning every conflict) and the hostable pane entries (palette rows
    // committing `plugin-pane`; placement `split`/`tab`/`zoomed` — overlay
    // is deferred and dropped with a warning). A broken manifest is skipped
    // with a warning; manifests resolve relative to the canonical config
    // path, the same resolution `phux config run` uses. Both derived
    // vectors are `mut` because the in-place config reload (phux-foz.5)
    // swaps them when a reload succeeds.
    let plugin_manifests: Vec<phux_config::plugin::PluginManifest> = loaded_cfg
        .as_ref()
        .map(|cfg| {
            phux_config::plugin::load_enabled_manifests(
                &phux_config::loader::config_path(),
                &cfg.plugins,
            )
        })
        .unwrap_or_default();
    let mut plugin_actions: Vec<PluginActionEntry> =
        plugin_actions::entries_from_manifests(&plugin_manifests);
    let mut plugin_panes: Vec<plugin_panes::PluginPaneEntry> =
        plugin_panes::entries_from_manifests(&plugin_manifests);
    // The plugin-events channel: spawned plugin-action tasks report
    // completion here; the select! arm below toasts failures. Sender is
    // lent to `DispatchCtx` each batch.
    let (plugin_tx, mut plugin_rx) = tokio::sync::mpsc::unbounded_channel::<PluginRunResult>();
    let mut keybindings_snapshot: Option<phux_config::KeybindingsCfg> =
        loaded_cfg.as_ref().map(|c| {
            let mut kb = c.keybindings.clone();
            plugin_actions::merge_plugin_bindings(&mut kb, &plugin_actions);
            kb
        });
    // phux-4li.5: keybind resolver, built from the plugin-merged snapshot
    // so a manifest `keys` chord resolves exactly like a user binding.
    // The resolver consumes `InputEvent::Key` events *before* they would
    // be forwarded to the focused pane; a chord that resolves to an
    // action mutates the active window here and never reaches the
    // server's input pipe.
    let mut resolver = keybindings_snapshot.as_ref().and_then(build_resolver_from);
    // phux-ahv.4: single source of truth for chrome + overlay colors,
    // owned alongside the keybindings snapshot and threaded into the
    // overlay render path via `DispatchCtx`.
    let mut theme: crate::render::Theme = loaded_cfg
        .as_ref()
        .map_or_else(crate::render::Theme::default, |c| {
            crate::render::Theme::from_cfg(&c.theme)
        });
    // phux-foz.1: the attention hint's chip color comes from the theme's
    // `attention` slot rather than a hardcoded SGR in the painter.
    if let Some(sb) = status_bar.as_mut() {
        sb.set_attention_color(theme.attention);
    }
    // phux-r82.6: spawn one bounded interval runner per `exec` widget. The
    // runners execute off-loop and write into the widgets' shared caches;
    // the bar's normal repaint tick picks changed cells up, so the render
    // loop never blocks on a widget command. The guard aborts the tasks
    // (and via kill_on_drop, their children) when this attach loop ends.
    let _exec_runners = spawn_exec_feed_runners(
        status_bar
            .as_ref()
            .map(StatusBarPainter::exec_feeds)
            .unwrap_or_default(),
    );
    // phux-4h5a: window-sidebar render state, driver-local like `zoomed`. The
    // `[sidebar]` config seeds the initial enabled flag, width, and edge; the
    // `toggle-sidebar` action flips `sidebar_enabled` at runtime. Each frame
    // `sidebar_reservation()` folds these into an `Option<SidebarReservation>`
    // that threads to every layout site, so panes, dividers, reflow, mouse, and
    // the strip itself agree on the same inset. Default-off keeps the disabled
    // path byte-identical.
    let sidebar_cfg = loaded_cfg.as_ref().map(|c| c.sidebar.clone());
    let mut sidebar_enabled = sidebar_cfg.as_ref().is_some_and(|c| c.enabled);
    let sidebar_width = sidebar_cfg.as_ref().map_or(20, |c| c.width);
    let sidebar_edge = match sidebar_cfg.as_ref().map(|c| c.position) {
        Some(SidebarPosition::Right) => SidebarEdge::Right,
        _ => SidebarEdge::Left,
    };
    // The strip painter, themed like the status bar. Fed `window_infos` from
    // the same snapshot that drives the tab strip; caches so an unchanged
    // repaint emits nothing.
    let mut sidebar_painter = SidebarPainter::new(theme);
    // phux-5ke.4: overlay state — initially empty. Pushed onto by the
    // `show-help` action; drained by `OverlayState::handle_key` when
    // the active overlay returns `Dismiss`. While active, key events
    // route to the overlay (no pane forwarding) and pane stdout flushes
    // are suppressed (ADR-0020 §Decision invariant 5).
    let mut overlays = OverlayState::new();
    // phux-oih5.16: one client-local return point for attention navigation.
    // Cycling never overwrites it; return consumes it. It is deliberately
    // absent from Workspace/L3 metadata and resets on re-attach.
    let mut attention_navigation = AttentionNavigation::default();
    // ADR-0048: the in-flight divider drag. `None` between drags; a press
    // on a divider records the grabbed split, motion re-tunes it, release
    // clears it. Lives across dispatch batches (press and release land in
    // different `select!` wakeups), so it is owned here and lent to
    // `DispatchCtx` by reference each batch.
    let mut drag: Option<super::input_dispatch::DragGrab> = None;
    // phux-npb3 (ADR-0048 decision 3 follow-up): per-pane mouse opt-out.
    // `set-pane mouse off` puts the focused pane in this set; the dispatcher
    // then never synthesizes INPUT_MOUSE for it, and the sync at the top of
    // each loop iteration drops the outer-terminal mouse-tracking DECSET
    // whenever the focused pane is opted out — so the host's raw mouse
    // handling (native selection etc.) returns for that pane alone while
    // sibling panes keep drag-to-resize. Client-local; nothing on the wire.
    // `mouse_capture_cfg` mirrors the global `mouse` gate the RawModeGuard
    // install used: with `mouse = false` capture stays off unconditionally.
    let mouse_capture_cfg = loaded_cfg.as_ref().is_none_or(|c| c.defaults.mouse);
    let mut mouse_optout: std::collections::HashSet<TerminalId> = std::collections::HashSet::new();
    // Track the current outer-terminal viewport so the painter knows
    // which row is "bottom". Initialized to a sensible default and
    // updated by SIGWINCH; the server doesn't drive client-side
    // viewport (clients own their chrome per DESIGN §8.5).
    let mut viewport_dims: (u16, u16) =
        current_viewport().map_or((80, 24), |v| (v.cols.max(1), v.rows.max(1)));
    // Host per-cell pixel size for the INPUT_MOUSE cells→pixels scaling
    // (SPEC input.md §3.1). Tracked next to `viewport_dims` and refreshed
    // on the same SIGWINCH edge — a monitor change can move the window to
    // a display with a different cell size (phux-yyex).
    let mut cell_px_dims: (u16, u16) =
        current_viewport().map_or(HOST_CELL_PX_FALLBACK, |v| host_cell_px(&v));
    let mut session_name = String::new();
    // phux-4li.20: cache of the server's session graph, refreshed from
    // every ATTACHED snapshot. The `<leader> a` session picker reads
    // this to list peer sessions; `focused_session` marks the row the
    // client is currently attached to (excluded from the picker).
    let mut sessions: Vec<phux_protocol::wire::info::SessionInfo> = Vec::new();
    let mut focused_session: Option<phux_protocol::ids::SessionId> = None;
    // phux-foz.8: peer sessions' persisted layouts, fetched right after the
    // session graph lands (one GET_METADATA per peer, correlated through
    // `foreign_layout_pending`). The window picker reads the cache to render
    // one-step cross-session window rows; sessions with no entry fall back
    // to the plain "switch to this session" row. Attach-time snapshot only —
    // we do not subscribe to peers' layout keys.
    let mut foreign_layouts: HashMap<phux_protocol::ids::SessionId, Workspace> = HashMap::new();
    let mut foreign_layout_pending: HashMap<u32, phux_protocol::ids::SessionId> = HashMap::new();
    // phux-jpqd: the `phux.agent/v1` records of FOREIGN panes, so the
    // agent-fleet dashboard shows a peer session's agent glyph/state without
    // attaching there. Populated lazily: when a peer's layout lands
    // (`apply_foreign_layout_reply`), the driver fires one GET_METADATA per
    // `TerminalId` in that workspace on the pane's agent key, correlated
    // through `foreign_agent_pending`. Keyed by foreign terminal id; pruned
    // to the union of all cached foreign layouts' leaves on each fold so it
    // stays bounded. No subscription — a one-shot read, same lazy-query
    // shape as the foreign layouts above (ADR-0018 / ADR-0030).
    let mut foreign_agents: HashMap<TerminalId, AgentRecord> = HashMap::new();
    let mut foreign_agent_pending: HashMap<u32, TerminalId> = HashMap::new();
    // phux-foz.8: the deferred window select of a one-step cross-session
    // pick, consumed on the first layout reconcile below. phux-jpqd:
    // `pending_pane` is the DFS leaf ordinal focused after the window
    // select resolves — the pane half of a one-step cross-session pick.
    let mut pending_window = initial_window;
    let mut pending_pane = initial_pane;
    let mut parser = StdinParser::new();
    // Predictive local echo (phux-9gw.1). State is updated alongside
    // every keystroke and drained on every TERMINAL_OUTPUT; when
    // `predict_cfg.enabled == false` every `predict_key` returns
    // `Disabled` so the overlay never paints.
    let mut predict = PredictionState::new(predict_cfg, 80, 24);
    let overlay = Overlay;
    let mut stdin = tokio::io::stdin();
    let mut stdin_buf = [0u8; 4096];
    let mut sigwinch = signal(SignalKind::window_change()).map_err(AttachError::Io)?;
    // `phux-roz`: SIGINT/SIGTERM/SIGHUP handlers run terminal cleanup
    // before exiting non-zero. SIGKILL is uncatchable; deferring
    // alt-screen entry until after handshake covers most real failure
    // modes for that case.
    let mut sigint = signal(SignalKind::interrupt()).map_err(AttachError::Io)?;
    let mut sigterm = signal(SignalKind::terminate()).map_err(AttachError::Io)?;
    let mut sighup = signal(SignalKind::hangup()).map_err(AttachError::Io)?;
    let mut detach_pending = false;
    // Bare-ESC disambiguation deadline, anchored to the iteration where the
    // parser first went pending. Re-creating the sleep each loop pass (the
    // pre-anchor behavior) restarted the full window whenever ANY other arm
    // fired first — under a steady output stream (status-line clock, shell
    // highlight repaints) a lone Escape could be deferred far past the
    // intended window. `None` ⇔ nothing pending.
    let mut esc_deadline: Option<tokio::time::Instant> = None;
    // phux-foz.2: which-key popup arming. When the resolver sits at the
    // pending-prefix state (`<prefix>` pressed, continuation awaited) for
    // `which_key_delay` without a follow-up chord, the loop pushes a
    // which-key overlay listing the prefix-table continuations. Config
    // comes from the same `[keybindings]` snapshot the help overlay uses;
    // with no loaded config there is no resolver (and so no prefix to
    // hesitate on), so the popup is naturally inert. `None` ⇔ not armed.
    // Same anchored-deadline pattern as `esc_deadline`: the deadline is
    // set once when the pending state is first observed and survives
    // unrelated arms firing, so a busy output stream cannot starve it.
    let mut which_key_enabled = keybindings_snapshot.as_ref().is_some_and(|kb| kb.which_key);
    let mut which_key_delay = Duration::from_millis(
        keybindings_snapshot
            .as_ref()
            .map_or(600, |kb| kb.which_key_delay_ms),
    );
    let mut which_key_deadline: Option<tokio::time::Instant> = None;
    // phux-eb0: set by `apply_action_effects` when the user commits a
    // `switch-session`. Checked after each input-dispatch batch; a value
    // here makes `main_loop` return `LoopExit::SwitchTo` so the outer
    // loop re-attaches to the named session on the same connection.
    let mut switch_request: Option<ReattachTarget> = None;
    // phux-foz.5: set by `apply_action_effects` when the user commits a
    // `reload-config` (palette or bound chord). Checked after each
    // input-dispatch batch; the driver then re-runs the layered config
    // loader and swaps its config-derived state in place — or keeps the
    // old state and toasts the error. The `phux config reload` CLI
    // doorbell reaches the same handler via `FrameOutcome::config_reload`.
    let mut reload_request = false;

    // Replay the `ATTACHED` frame so the focused-pane bookkeeping in
    // `handle_server_frame` runs exactly once, in one place. The sidebar
    // reservation for this bootstrap frame (recomputed per-iteration in the
    // loop below to track `toggle-sidebar`).
    let sidebar = sidebar_enabled.then_some(SidebarReservation {
        edge: sidebar_edge,
        width: sidebar_width,
    });
    let outcome = handle_server_frame(
        out,
        initial_attached,
        &mut panes,
        &mut workspace,
        &mut focused_pane,
        &mut zoomed,
        &mut session_name,
        status_bar.as_mut(),
        sidebar,
        viewport_dims,
        &mut predict,
        &overlay,
        layout_get_request_id,
        &mut pending_splits,
        &mut pending_windows,
        &mut agent_meta,
        overlays.is_active(),
        // Single replayed frame — no burst to coalesce, paint it.
        false,
    )?;
    if outcome.exit {
        return Ok(LoopExit::Detached);
    }
    vcs.apply_snapshot(outcome.pane_cwds);
    if let Some((list, focused)) = outcome.sessions {
        sessions = list;
        focused_session = Some(focused);
    }
    // phux-foz.8: fetch each peer session's persisted layout so the window
    // picker can list foreign windows as one-step jump rows. Fire-and-forget:
    // replies drain through the recv arm below; a peer with nothing persisted
    // never replies with a value and simply keeps its fallback row.
    request_foreign_layouts(
        conn,
        &sessions,
        focused_session,
        &mut next_request_id,
        &mut foreign_layout_pending,
    )
    .await?;
    // ADR-0033: cache our own ClientId (for the "you hold the wheel" badge) and
    // opt into the agent-event stream so `TerminalControl` broadcasts (lease +
    // lifecycle) reach this client. Server-scoped (`terminal: None`) so we see
    // control events for every pane, not just one.
    if outcome.own_client_id.is_some() {
        own_client_id = outcome.own_client_id;
    }
    conn.send(&FrameKind::SubscribeEvents { terminal: None })
        .await?;
    // phux-foz.5: watch the config-reload doorbell so a `phux config
    // reload` from any shell reaches this client as a METADATA_CHANGED
    // broadcast (the config itself never crosses the wire — we re-read
    // our own file). Torn down implicitly on detach like every metadata
    // subscription.
    conn.send(&FrameKind::SubscribeMetadata {
        scope: Scope::Global,
        key: CONFIG_RELOAD_KEY.to_owned(),
    })
    .await?;
    if outcome.subscribe_layout
        && let Some(session) = focused_session
    {
        // phux-4li.5: ask the server for any persisted layout, then
        // subscribe to future mutations. Both frames are best-effort —
        // if the server rejects them with an ERROR (we'd see one in a
        // later loop iteration) we just stay in the single-pane
        // bootstrap. phux-jy4t: keyed per session so we restore THIS
        // session's layout, not whatever sibling wrote the key last.
        let key = layout_key(session);
        let req_id = next_request_id;
        layout_get_request_id = Some(req_id);
        next_request_id = next_request_id.wrapping_add(1);
        conn.send(&FrameKind::GetMetadata {
            request_id: req_id,
            scope: Scope::Group(DEFAULT_GROUP_ID),
            key: key.clone(),
        })
        .await?;
        conn.send(&FrameKind::SubscribeMetadata {
            scope: Scope::Group(DEFAULT_GROUP_ID),
            key,
        })
        .await?;
    }
    // ADR-0040: read + watch every bootstrap pane's `phux.agent/v1` record
    // so window labels can prefer structured agent identity from the first
    // paint. The same sweep re-runs whenever the pane set changes.
    sync_agent_meta_subscriptions(
        conn,
        panes.keys().cloned().collect(),
        &mut agent_meta,
        &mut next_request_id,
    )
    .await?;
    // phux-4li.17: seed the window/tab strip from the bootstrap layout so
    // the first bar paint (driven by TERMINAL_SNAPSHOT) shows the window.
    // phux-4h5a: the sidebar painter tracks the same window list so the strip's
    // tab list stays current whenever the bar's does.
    {
        refresh_window_chrome(
            status_bar.as_mut(),
            &mut sidebar_painter,
            &workspace,
            &panes,
            focused_pane.as_ref(),
            zoomed.as_ref(),
            own_client_id,
            &agent_meta,
            &mut vcs,
        );
    }

    // phux-foz.6: first-run onboarding hint. The caller already applied the
    // trigger rule (nothing exists at the canonical config path; once per
    // attach invocation — see `super::onboarding`); here we push the notice
    // onto the ordinary overlay stack AFTER the bootstrap frame painted, so
    // it floats over the live pane like any other modal. It reuses
    // `ToastOverlay`, so any key dismisses it and triggers the standard
    // dismiss-repaint; while it is up, frames keep applying to the pane
    // mirrors with the outbound flush paused (ADR-0020 invariant 5), exactly
    // as for every other overlay.
    if show_onboarding {
        overlays.push(Box::new(crate::render::overlay::ToastOverlay::new(
            super::onboarding::ONBOARDING_TITLE,
            super::onboarding::hint_lines(&phux_config::loader::config_path()),
            &theme,
        )));
        paint_active_overlay(
            out,
            &overlays,
            &workspace,
            &mut panes,
            focused_pane.as_ref(),
            zoomed.as_ref(),
            viewport_dims,
            status_bar.as_mut(),
            sidebar,
            Some(&mut sidebar_painter),
            &session_name,
            &theme,
        );
    }

    loop {
        // phux-4h5a: fold the driver-local sidebar render state into the
        // per-frame reservation threaded to every layout site this iteration.
        // `toggle-sidebar` flips `sidebar_enabled`; the change takes effect on
        // the next iteration. `None` (the default) keeps `content_rect` the
        // full pane viewport, so the whole path is byte-identical when the
        // sidebar is off.
        let sidebar = sidebar_enabled.then_some(SidebarReservation {
            edge: sidebar_edge,
            width: sidebar_width,
        });
        // phux-npb3: capture follows focus. Re-derive the outer-terminal
        // mouse-tracking DECSET from the focused pane's opt-out state every
        // iteration — one call site covers every way focus or the set can
        // change (set-pane, click-to-focus, keybind navigation, spawn/close
        // reflows). `sync_mouse_capture` is a no-op when nothing changed, so
        // the steady-state cost is one bool compare. Closed panes are pruned
        // so a recycled TerminalId can never inherit a stale opt-out.
        if !mouse_optout.is_empty() {
            mouse_optout.retain(|id| panes.contains_key(id));
        }
        // The attention ladder's `seen` half: the pane the user is looking at
        // has, by definition, been looked at. One hash lookup per iteration —
        // and it covers EVERY way focus can move (click, keybind, split,
        // window switch, a peer's layout broadcast) without a call at each
        // site. A later agent-state change on an unfocused pane re-arms the
        // bit (see `server_frame::note_agent_change`), which is what lets a
        // background agent's `done` climb back above the working ones.
        //
        // The FLIP is a chrome trigger, not a silent side effect. The focus
        // action that made this pane focused ran in the PREVIOUS iteration, and
        // it recomputed the chrome while `seen` was still false — so the strip
        // it painted still carries the filled "look at me" diamond, bold,
        // pinned above every working agent, about the very pane the user is now
        // looking at. Nothing else recomputes `agent_entries` (the status tick
        // paints only the bar), so without this the row keeps lying until some
        // unrelated chrome event happens to fire — indefinitely, in a
        // single-agent session. That defeats the ladder's central promise:
        // visiting a pane demotes it.
        if mark_focused_seen(&mut panes, focused_pane.as_ref()) {
            let chrome_changed = refresh_window_chrome(
                status_bar.as_mut(),
                &mut sidebar_painter,
                &workspace,
                &panes,
                focused_pane.as_ref(),
                zoomed.as_ref(),
                own_client_id,
                &agent_meta,
                &mut vcs,
            );
            // ADR-0029: demoting a ladder row touches no pane interior, so this
            // is an in-place CHROME paint, never a full-frame clear. Gated on
            // the painter's own change report, so a focus change that moves no
            // agent row costs zero bytes.
            if chrome_changed
                && !overlays.is_active()
                && let Some(ls) = workspace.render_window(zoomed.as_ref()).as_deref()
            {
                paint_chrome_in_place(
                    out,
                    ls,
                    &panes,
                    focused_pane.as_ref(),
                    viewport_dims,
                    status_bar.as_mut(),
                    sidebar,
                    Some(&mut sidebar_painter),
                    &session_name,
                );
            }
        }
        let want_capture =
            desired_mouse_capture(mouse_capture_cfg, focused_pane.as_ref(), &mouse_optout);
        sync_mouse_capture(out, want_capture).map_err(AttachError::Io)?;
        // phux-wrnm: hover reporting follows the overlay stack the same way
        // capture follows focus — raised while a context menu wants to track
        // the pointer with no button held, dropped as soon as it closes.
        sync_hover_tracking(out, overlays.wants_pointer_hover()).map_err(AttachError::Io)?;
        // phux-fysb: the off-loop stdout writer dropped a stale backlog under
        // a slow terminal. Repaint the latest state from scratch — a
        // self-contained full frame (or overlay) supersedes the dropped
        // diffs. `swap(false)` clears the flag, but any set re-armed by THIS
        // repaint's own flushes is preserved for the next iteration. Checked
        // before parking so a resync that landed during the prior arm is
        // serviced promptly.
        if needs_resync.is_some_and(|flag| flag.swap(false, Ordering::AcqRel)) {
            if overlays.is_active() {
                paint_active_overlay(
                    out,
                    &overlays,
                    &workspace,
                    &mut panes,
                    focused_pane.as_ref(),
                    zoomed.as_ref(),
                    viewport_dims,
                    status_bar.as_mut(),
                    sidebar,
                    Some(&mut sidebar_painter),
                    &session_name,
                    &theme,
                );
            } else if let Some(ls) = workspace.render_window(zoomed.as_ref()).as_deref() {
                paint_full_frame(
                    out,
                    ls,
                    &mut panes,
                    focused_pane.as_ref(),
                    viewport_dims,
                    status_bar.as_mut(),
                    sidebar,
                    Some(&mut sidebar_painter),
                    &session_name,
                );
            }
        }

        // Arm the bare-ESC idle timer only when the parser has pending
        // state, anchored to the first iteration that saw it (the deadline
        // survives other arms firing — see `esc_deadline`). When no flush
        // is pending we substitute a never-resolving future so the select!
        // arm parks forever; this keeps the steady-state cost at one
        // always-`Pending` future and avoids unused-`Option` branches
        // inside `select!`.
        if parser.has_pending() {
            esc_deadline.get_or_insert_with(|| tokio::time::Instant::now() + ESC_FLUSH_IDLE);
        } else {
            esc_deadline = None;
        }
        let flush_sleep: std::pin::Pin<Box<dyn Future<Output = ()>>> = match esc_deadline {
            Some(deadline) => Box::pin(tokio::time::sleep_until(deadline)),
            None => Box::pin(std::future::pending::<()>()),
        };

        // phux-foz.2: (dis)arm the which-key deadline from the resolver's
        // CURRENT pending state. An early continuation chord (dispatched
        // in the stdin arm) leaves the resolver non-pending, so the next
        // pass through here disarms the timer before it can fire — the
        // popup is suppressed without any explicit cancellation call.
        update_which_key_deadline(
            &mut which_key_deadline,
            resolver
                .as_ref()
                .is_some_and(phux_config::keybind::Resolver::pending_at_prefix),
            which_key_enabled,
            overlays.is_active(),
            tokio::time::Instant::now(),
            which_key_delay,
        );
        let which_key_sleep: std::pin::Pin<Box<dyn Future<Output = ()>>> = match which_key_deadline
        {
            Some(deadline) => Box::pin(tokio::time::sleep_until(deadline)),
            None => Box::pin(std::future::pending::<()>()),
        };

        // phux-nz4.5: per-bar repaint cadence. Driven by the slowest
        // widget that wants periodic refresh (currently floor-1s via the
        // `time` widget). Empty bar ⇒ `Pending` forever so this select!
        // arm never fires.
        let status_tick: std::pin::Pin<Box<dyn Future<Output = ()>>> = match status_bar
            .as_ref()
            .and_then(StatusBarPainter::min_poll_interval)
        {
            Some(interval) => Box::pin(tokio::time::sleep(interval)),
            None => Box::pin(std::future::pending::<()>()),
        };

        // Synchronized-output transactions intentionally span arbitrary
        // socket reads, so their deadline is pane state rather than a
        // per-batch timer. A stuck producer gets one bounded recovery paint;
        // later bytes re-arm suppression if mode 2026 is still set.
        let sync_output_sleep: std::pin::Pin<Box<dyn Future<Output = ()>>> = panes
            .values()
            .filter_map(|slot| slot.sync_output_since)
            .map(|since| since + SYNC_OUTPUT_WATCHDOG)
            .min()
            .map_or_else(
                || Box::pin(std::future::pending::<()>()) as _,
                |deadline| Box::pin(tokio::time::sleep_until(deadline)) as _,
            );

        tokio::select! {
            biased;

            // Stdin is polled before inbound frames so a local keystroke
            // is dispatched promptly rather than waiting behind an output
            // burst. One read is bounded by `stdin_buf`; the inbound arm is
            // bounded by `FRAME_COALESCE_CAP`, so neither starves the other.
            n = stdin.read(&mut stdin_buf) => {
                let n = n.map_err(AttachError::Io)?;
                if n == 0 {
                    // Stdin EOF — outer terminal closed. Detach cleanly.
                    if !detach_pending {
                        conn.send(&FrameKind::Detach).await?;
                        detach_pending = true;
                    }
                    continue;
                }
                let events = parser.feed(&stdin_buf[..n]);
                // Capture the pre-dispatch view so zoom and sidebar toggles can
                // diff against it and resize each changed pane's PTY. Taken
                // before dispatch mutates either piece of view geometry.
                let prev_zoomed = zoomed.clone();
                let prev_sidebar = sidebar;
                let prev_view_rects = view_rects(
                    &workspace,
                    prev_zoomed.as_ref(),
                    content_rect(
                        viewport_dims,
                        status_bar.as_ref().map(StatusBarPainter::position),
                        sidebar,
                    ),
                    viewport_dims,
                );
                // phux-foz.9: the agents-section row -> window mapping,
                // snapshotted from the strip painter so a click on an
                // agent row hit-tests against exactly what was painted.
                let sidebar_agent_rows = sidebar_painter.agent_windows();
                let mut ctx = DispatchCtx {
                    resolver: resolver.as_mut(),
                    focus_history: focus_history.clone(),
                    workspace: &mut workspace,
                    viewport: viewport_dims,
                    cell_px: cell_px_dims,
                    next_request_id: &mut next_request_id,
                    pending_splits: &mut pending_splits,
                    pending_windows: &mut pending_windows,
                    overlays: &mut overlays,
                    keybindings: keybindings_snapshot.as_ref(),
                    theme: &theme,
                    sessions: &sessions,
                    foreign_layouts: &foreign_layouts,
                    foreign_agents: &foreign_agents,
                    focused_session,
                    session_name: &mut session_name,
                    switch_request: &mut switch_request,
                    zoomed: &mut zoomed,
                    sidebar,
                    sidebar_enabled: &mut sidebar_enabled,
                    sidebar_agents: &sidebar_agent_rows,
                    bar: status_bar.as_ref().map(StatusBarPainter::position),
                    status_bar: status_bar.as_ref(),
                    drag: &mut drag,
                    mouse_optout: &mut mouse_optout,
                    attention_navigation: &mut attention_navigation,
                    plugin_actions: &plugin_actions,
                    plugin_panes: &plugin_panes,
                    plugin_tx: Some(&plugin_tx),
                    reload_request: &mut reload_request,
                    agent_meta: &agent_meta.records,
                    vcs: &mut vcs,
                };
                let layout_changed = dispatch_input_events(
                    out,
                    conn,
                    events,
                    &mut focused_pane,
                    &mut detach_pending,
                    &mut predict,
                    &overlay,
                    &mut panes,
                    &mut ctx,
                )
                .await?;
                focus_history = ctx.focus_history;
                // phux-4h5a: a `toggle-sidebar` in this batch flipped
                // `sidebar_enabled`. Re-fold it into the reservation so the
                // reflow + repaint below tile into the NEW content rect this
                // iteration rather than waiting a frame.
                let sidebar = sidebar_enabled.then_some(SidebarReservation {
                    edge: sidebar_edge,
                    width: sidebar_width,
                });
                // phux-eb0: a committed `switch-session` ends this loop so
                // the outer driver re-attaches. Return BEFORE any repaint
                // — the new session's ATTACHED + snapshot will repaint.
                if let Some(target) = switch_request.take() {
                    return Ok(LoopExit::SwitchTo(target));
                }
                // Zoom and sidebar toggles both change pane geometry. Resize
                // every affected PTY before repainting so applications reflow
                // to the same rectangle the client is about to render.
                if zoomed != prev_zoomed || sidebar != prev_sidebar {
                    emit_view_reflow(
                        conn,
                        &workspace,
                        zoomed.as_ref(),
                        &prev_view_rects,
                        content_rect(
                        viewport_dims,
                        status_bar.as_ref().map(StatusBarPainter::position),
                        sidebar,
                    ),
                    )
                    .await?;
                }
                if layout_changed {
                    // ADR-0040: an input action may have split/closed panes;
                    // keep the agent-metadata watches in step with the set.
                    sync_agent_meta_subscriptions(
                        conn,
                        panes.keys().cloned().collect(),
                        &mut agent_meta,
                        &mut next_request_id,
                    )
                    .await?;
                    refresh_window_chrome(
                        status_bar.as_mut(),
                        &mut sidebar_painter,
                        &workspace,
                        &panes,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        own_client_id,
                        &agent_meta,
                    &mut vcs,
                    );
                    // phux-5ke.4: on overlay dismiss the dispatcher
                    // sets layout_changed=true; the full-frame repaint
                    // below restores pane content under the now-gone
                    // modal. When the overlay is still active (e.g.
                    // a push happened in the same batch) we skip the
                    // pane repaint and go straight to overlay paint.
                    if !overlays.is_active()
                        && let Some(ls) = workspace.render_window(zoomed.as_ref()).as_deref()
                    {
                        paint_full_frame(
                            out,
                            ls,
                            &mut panes,
                            focused_pane.as_ref(),
                            viewport_dims,
                            status_bar.as_mut(),
                            sidebar,
                            Some(&mut sidebar_painter),
                            &session_name,
                        );
                    }
                }
                if overlays.is_active() {
                    paint_active_overlay(
                        out,
                        &overlays,
                        &workspace,
                        &mut panes,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                        &theme,
                    );
                }
                // phux-foz.5: a `reload-config` committed in this batch
                // (palette row or bound chord). Runs LAST in the arm so
                // its repaint reflects the new theme/bar.
                if reload_request {
                    reload_request = false;
                    handle_config_reload(
                        out,
                        &mut keybindings_snapshot,
                        &mut resolver,
                        &mut theme,
                        &mut status_bar,
                        &mut sidebar_painter,
                        &mut plugin_actions,
                        &mut plugin_panes,
                        &mut which_key_enabled,
                        &mut which_key_delay,
                        &mut overlays,
                        &workspace,
                        &mut panes,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        own_client_id,
                        &agent_meta,
                        &mut vcs,
                        viewport_dims,
                        sidebar,
                        &session_name,
                    );
                }
            }

            // Inbound frames are drained in a `FRAME_COALESCE_CAP`-bounded
            // batch so a redraw burst paints once; bounded so it cannot
            // starve the stdin arm polled above it.
            frame = conn.recv() => {
                match frame {
                    Ok(first) => {
                        // phux-jhv8: drain every frame already queued so a
                        // back-to-back output burst (nvim startup, a
                        // full-screen redraw) applies all its vt_writes and
                        // paints ONCE — on the final frame — instead of a
                        // render + blocking flush per frame. The non-blocking
                        // try_recv stops the moment the socket would block, so
                        // a lone frame keeps the old one-frame-one-paint path.
                        let mut batch = vec![first];
                        while batch.len() < FRAME_COALESCE_CAP {
                            match conn.try_recv() {
                                Ok(Some(more)) => batch.push(more),
                                // Socket drained, or a clean EOF the next
                                // `recv()` will surface as Disconnected.
                                Ok(None) | Err(AttachError::Disconnected) => break,
                                Err(err) => return Err(err),
                            }
                        }
                        // Per-pane last-wins: a frame defers its paint iff a
                        // LATER frame in the burst repaints the same pane, so
                        // every touched pane (focused or not) settles exactly
                        // once on its final frame. No pane is left stale, and
                        // the hot single-pane case collapses to one paint.
                        let paint_targets: Vec<Option<TerminalId>> = batch
                            .iter()
                            .map(|f| frame_paint_target(f).cloned())
                            .collect();
                        let defer_flags = coalesce_defer_flags(&paint_targets);
                        // ADR-0029 §2: the loop-level repaint triggers in this
                        // batch RAISE a level instead of painting inline, and
                        // the accumulator is drained ONCE below. A burst of
                        // twenty `MetadataChanged` frames (a live agent
                        // detector publishing state transitions across nine
                        // panes) therefore collapses into a single in-place
                        // sidebar paint rather than twenty full-screen clears.
                        // Declared HERE, inside the frame arm, deliberately:
                        // the stdin / ESC-flush arms shadow `sidebar` with a
                        // freshly recomputed reservation so a same-iteration
                        // `toggle-sidebar` takes effect, and a drain hoisted
                        // outside the `select!` would capture the stale outer
                        // one. This arm does not shadow it.
                        let mut repaint = RepaintAccumulator::default();
                        for (frame_idx, f) in batch.into_iter().enumerate() {
                        // phux-foz.8: a peer session's persisted-layout GET
                        // reply. Picker/fleet display data only — decode into
                        // the cache and skip the general frame handler (whose
                        // MetadataValue arm would drop the unmatched id).
                        // phux-jpqd: once a peer's pane tree is known, fetch
                        // each pane's agent record (prune stale first) so the
                        // fleet dashboard's foreign rows carry agent state,
                        // then refresh a live fleet in place.
                        let f = match f {
                            FrameKind::MetadataValue { request_id, value }
                                if foreign_layout_pending.contains_key(&request_id) =>
                            {
                                if let Some(session) = foreign_layout_pending.remove(&request_id) {
                                    apply_foreign_layout_reply(
                                        &mut foreign_layouts,
                                        session,
                                        value.as_deref(),
                                    );
                                    prune_foreign_agents(&mut foreign_agents, &foreign_layouts);
                                    if let Some(ws) = foreign_layouts.get(&session) {
                                        request_foreign_agents(
                                            conn,
                                            ws,
                                            &mut next_request_id,
                                            &mut foreign_agent_pending,
                                        )
                                        .await?;
                                    }
                                    // ADR-0029 §2: raise, drain once (below the
                                    // loop). A peer's layout reply arrives with
                                    // one agent-record reply per foreign pane
                                    // right behind it; refreshing inline would
                                    // re-project (and repaint) the dashboard
                                    // once per reply.
                                    repaint.raise_fleet();
                                }
                                continue;
                            }
                            // phux-jpqd: a foreign pane's agent-record GET
                            // reply. Fold into the fleet cache and refresh a
                            // live fleet; same intercept shape as the layout
                            // reply (the general handler would drop it).
                            FrameKind::MetadataValue { request_id, value }
                                if foreign_agent_pending.contains_key(&request_id) =>
                            {
                                if let Some(id) = foreign_agent_pending.remove(&request_id) {
                                    apply_foreign_agent_reply(
                                        &mut foreign_agents,
                                        id,
                                        value.as_deref(),
                                    );
                                    repaint.raise_fleet();
                                }
                                continue;
                            }
                            // phux-h5hj.12: the same two lookups for the
                            // *refusal* shape. `proto.md` §9 lets a server
                            // answer a request it will not serve with a
                            // correlated ERROR instead of the reply frame,
                            // and a peer session's Group is exactly the kind
                            // of scope a policy refuses. Without this arm the
                            // pending entry is never removed: the row stays
                            // blank for the life of the attach, the map grows
                            // by one per refused read, and the ERROR falls
                            // through to `handle_server_frame` as if it were
                            // an unrelated notice. Dropping the entry is the
                            // whole fix — a refused read has no value to
                            // apply, and the fleet projection already renders
                            // a session it knows nothing about.
                            FrameKind::Error {
                                request_id: Some(request_id),
                                ..
                            } if foreign_layout_pending.contains_key(&request_id)
                                || foreign_agent_pending.contains_key(&request_id) =>
                            {
                                foreign_layout_pending.remove(&request_id);
                                foreign_agent_pending.remove(&request_id);
                                continue;
                            }
                            other => other,
                        };
                        let defer_paint = frame_defers_paint(defer_flags[frame_idx], &f);
                        // phux-tnh: snapshot the current per-leaf rects
                        // BEFORE the frame may fold (close) or split the
                        // layout, so a TerminalClosed/Spawned can diff
                        // against them and resize survivors whose dims
                        // changed. Only meaningful in multi-pane mode;
                        // skipped (no cost) on the single-pane hot path.
                        // phux-x2hm: snapshot the zoom-honoring rects so a
                        // close/spawn diffs against what is actually on screen;
                        // a TerminalSpawned-ok un-zooms (sets `zoomed = None`)
                        // inside `handle_server_frame`, so the post-frame view
                        // below correctly reflows every pane back to its tile.
                        let prev_rects = workspace
                            .render_window(zoomed.as_ref())
                            .and_then(|ls| {
                                ls.tree.as_ref().map(|_| {
                                    super::multi_pane::compute_layout_in(
                                        ls.as_ref(),
                                        content_rect(
                                            viewport_dims,
                                            status_bar.as_ref().map(StatusBarPainter::position),
                                            sidebar,
                                        ),
                                        viewport_dims,
                                    )
                                    .rects
                                })
                            });
                        let focused_before_frame = focused_pane.clone();
                        let outcome = handle_server_frame(
                            out,
                            f,
                            &mut panes,
                            &mut workspace,
                            &mut focused_pane,
                            &mut zoomed,
                            &mut session_name,
                            status_bar.as_mut(),
                            sidebar,
                            viewport_dims,
                            &mut predict,
                            &overlay,
                            layout_get_request_id,
                            &mut pending_splits,
                            &mut pending_windows,
                            &mut agent_meta,
                            overlays.is_active(),
                            defer_paint,
                        )?;
                        focus_history.observe(focused_before_frame, focused_pane.as_ref());
                        focus_history.repair(focused_pane.as_ref(), &workspace);
                        if outcome.exit {
                            return Ok(LoopExit::Detached);
                        }
                        // A peer headless placement can add a layout leaf
                        // without this attached client being subscribed to the
                        // new Terminal. Attach each discovered leaf so its
                        // snapshot creates a PaneSlot and renders in place.
                        for terminal_id in &outcome.attach_panes {
                            let request_id = next_request_id;
                            next_request_id = next_request_id.wrapping_add(1);
                            conn.send(&FrameKind::Command {
                                request_id,
                                command: Command::AttachTerminal {
                                    terminal_id: terminal_id.clone(),
                                },
                            })
                            .await?;
                        }
                        // phux-foz.7: did this frame change anything the
                        // agent-fleet dashboard projects (agent records,
                        // asked/lease state, layout/pane set, session
                        // graph)? Captured before the move-y outcome
                        // fields are consumed below; acted on after the
                        // per-frame handling (the fleet refresh block).
                        let fleet_dirty = outcome.chrome_dirty
                            || outcome.agent_meta_changed
                            || outcome.layout_replaced
                            || outcome.reflow_panes
                            || outcome.sessions.is_some();
                        // ADR-0040: the frame may have added panes
                        // (TerminalSpawned, a peer's layout broadcast) or
                        // removed them (TerminalClosed). Re-sweep so every
                        // live pane has a `phux.agent/v1` watch; the len
                        // guard keeps the steady state zero-cost.
                        if panes.len() != agent_meta.subscribed.len() {
                            sync_agent_meta_subscriptions(
                                conn,
                                panes.keys().cloned().collect(),
                                &mut agent_meta,
                                &mut next_request_id,
                            )
                            .await?;
                        }
                        // phux-4li.20: refresh the cached session graph
                        // whenever an ATTACHED snapshot lands so the
                        // session picker lists the current peer set.
                        // phux-p4vp: the same snapshot refreshes the
                        // pane-cwd index behind the sidebar branch line.
                        vcs.apply_snapshot(outcome.pane_cwds);
                        // phux-foz.8: re-request the peers' persisted
                        // layouts against the fresh graph so the window
                        // picker's one-step rows track it; replies
                        // overwrite stale cache entries.
                        if let Some((list, focused)) = outcome.sessions {
                            sessions = list;
                            focused_session = Some(focused);
                            request_foreign_layouts(
                                conn,
                                &sessions,
                                focused_session,
                                &mut next_request_id,
                                &mut foreign_layout_pending,
                            )
                            .await?;
                        }
                        // ADR-0033 / phux-foz.1: a `TerminalControl` or `Asked`
                        // event changed a pane's lease/lifecycle/attention. The
                        // event frame paints nothing, so refresh the chrome
                        // (supervisory badge, attention hint, window markers)
                        // and repaint here — but only when a painter input
                        // actually changed (`refresh_window_chrome` reports
                        // it), so an event that alters no visible state doesn't
                        // force a full-window repaint. (`own_client_id` is
                        // fixed for the life of this loop; it was captured at
                        // bootstrap.)
                        if outcome.chrome_dirty {
                            let chrome_changed = refresh_window_chrome(
                                status_bar.as_mut(),
                                &mut sidebar_painter,
                                &workspace,
                                &panes,
                                focused_pane.as_ref(),
                                zoomed.as_ref(),
                                own_client_id,
                                &agent_meta,
                            &mut vcs,
                            );
                            // ADR-0029: nothing about a title / lease /
                            // attention change touches a pane interior, so this
                            // is a CHROME raise, not a full-frame clear.
                            if chrome_changed && !overlays.is_active() {
                                repaint.raise_chrome();
                            }
                        }
                        // phux-3uv / ADR-0018: ack the applied TERMINAL_OUTPUT
                        // so the server's per-consumer SnapshotSynthesizer
                        // clears the dirty bits that produced this frame
                        // (mark_synced) and the next state-sync tick re-diffs
                        // against the acked reference. Without this the
                        // server re-emits an ever-growing unacked delta
                        // forever (see `tick_emit`). Cumulative per SPEC §12.2.
                        // Gated on the negotiated mode: a raw consumer's acks
                        // are dropped server-side, so skipping them removes a
                        // wasted write on the keystroke-carrying UDS during a
                        // repaint burst.
                        if let Some((terminal_id, seq)) =
                            should_emit_frame_ack(wants_state_sync, outcome.ack)
                        {
                            conn.send(&FrameKind::FrameAck { terminal_id, seq }).await?;
                        }
                        // phux-4li.12: a layout mutation triggered by a
                        // server frame (TerminalSpawned ok, TerminalClosed)
                        // requires the same `SET_METADATA` broadcast as
                        // a local action — see `ActionEffects.set_metadata`
                        // for the local-action path.
                        if outcome.emit_set_metadata
                            && let Some(session) = focused_session
                            && let Some(bytes) = encode_layout_or_log(&workspace)
                        {
                            let request_id = next_request_id;
                            next_request_id = next_request_id.wrapping_add(1);
                            conn.send(&FrameKind::SetMetadata {
                                request_id,
                                scope: Scope::Group(DEFAULT_GROUP_ID),
                                key: layout_key(session),
                                value: bytes,
                            })
                            .await?;
                        }
                        // phux-tnh: a pane close/spawn changed surviving
                        // panes' dimensions. Diff the folded/split layout
                        // against the pre-frame rects and emit a
                        // TERMINAL_RESIZE per changed leaf — same path the
                        // SIGWINCH arm uses — so the server reflows each
                        // PTY (TIOCSWINSZ) and the shell redraws to fill.
                        // Without this the survivor of a close keeps its
                        // old small winsize ("survivor stays small").
                        // Sent BEFORE the repaint so the server's resync
                        // snapshot lands after the local mirror has grown.
                        if outcome.reflow_panes
                            && let Some(prev_rects) = &prev_rects
                            && let Some(ls) = workspace.render_window(zoomed.as_ref())
                            && ls.tree.is_some()
                        {
                            let new_content =
                                content_rect(
                        viewport_dims,
                        status_bar.as_ref().map(StatusBarPainter::position),
                        sidebar,
                    );
                            let diff = super::reflow::compute_reflow(
                                ls.as_ref(),
                                prev_rects,
                                new_content,
                            );
                            for (terminal_id, new_rect) in &diff.changed {
                                conn.send(&FrameKind::TerminalResize {
                                    terminal_id: terminal_id.clone(),
                                    cols: new_rect.w,
                                    rows: new_rect.h,
                                })
                                .await?;
                            }
                        }
                        if outcome.layout_replaced {
                            // phux-foz.8: a one-step cross-session window
                            // pick drove this attach; the multi-window
                            // layout just landed, so resolve the deferred
                            // select against it before the repaint below.
                            // Out-of-range (a peer mutated the layout
                            // between pick and load) keeps the session's
                            // restored focus with a warning.
                            if let Some(idx) = pending_window.take() {
                                if workspace.select(idx) {
                                    let next_focus = workspace
                                        .active_window()
                                        .and_then(|ls| ls.focus.clone());
                                    focus_history.transition(&mut focused_pane, next_focus);
                                    // phux-jpqd: the pane half of a
                                    // one-step cross-session pane pick — move
                                    // focus onto the target DFS leaf of the
                                    // just-selected window. Out-of-range
                                    // (peer mutated the layout) keeps the
                                    // window's restored focus, logged.
                                    if let Some(ord) = pending_pane.take() {
                                        if let Some(leaf) = workspace
                                            .active_window()
                                            .and_then(|ls| ls.tree.as_ref())
                                            .map(crate::layout::leaves)
                                            .and_then(|leaves| leaves.get(ord).cloned())
                                        {
                                            if let Some(ls) = workspace.active_window_mut()
                                            {
                                                ls.focus = Some(leaf.clone());
                                            }
                                            focus_history
                                                .transition(&mut focused_pane, Some(leaf));
                                        } else {
                                            tracing::warn!(
                                                window = idx,
                                                pane = ord,
                                                "cross-session pane pick out of range; keeping window focus",
                                            );
                                        }
                                    }
                                    if let Some(fid) = focused_pane.as_ref() {
                                        reanchor_predict_to_pane(&mut predict, &panes, fid);
                                    }
                                } else {
                                    tracing::warn!(
                                        index = idx,
                                        windows = workspace.windows.len(),
                                        "cross-session window pick out of range; keeping restored focus",
                                    );
                                }
                            }
                            // phux-4li.5: layout changed under us
                            // (either the GET reply or a peer's broadcast).
                            // Trigger a full repaint: clear screen + paint
                            // dividers + re-render every pane.
                            // phux-5ke.4: while an overlay is up, defer
                            // the repaint — the dismiss path always
                            // triggers paint_full_frame, and the
                            // libghostty mirror is already updated.
                            refresh_window_chrome(
                                status_bar.as_mut(),
                                &mut sidebar_painter,
                                &workspace,
                                &panes,
                                focused_pane.as_ref(),
                                zoomed.as_ref(),
                                own_client_id,
                                &agent_meta,
                            &mut vcs,
                            );
                            // The pane rects moved: only a full-viewport
                            // repaint (ED2 + every pane + dividers) is a
                            // coherent base. ADR-0029: raise, drain once.
                            if !overlays.is_active() {
                                repaint.raise_full();
                            }
                            // The GET reply is single-use; clear the
                            // pending request id so a stray late
                            // MetadataValue can't trample state.
                            layout_get_request_id = None;
                        }
                        // ADR-0040: a `phux.agent/v1` record changed (GET
                        // reply or subscribed broadcast). The window labels
                        // and the sidebar's agents section derive from it, so
                        // recompose the chrome and schedule an IN-PLACE chrome
                        // paint.
                        //
                        // This arm used to call `paint_full_frame`
                        // UNCONDITIONALLY — no gate on whether a painter input
                        // actually changed, unlike the `chrome_dirty` arm. That
                        // was invisible only because nothing ever wrote the
                        // record, so the arm never fired. With a server-side
                        // agent-state detector publishing transitions, an
                        // ungated `paint_full_frame` here is an `ESC[2J`
                        // full-screen clear per transition. Both halves of the
                        // fix are required: gate on `refresh_window_chrome`'s
                        // change report, AND route to the in-place chrome
                        // painter via the accumulator.
                        if outcome.agent_meta_changed {
                            let chrome_changed = refresh_window_chrome(
                                status_bar.as_mut(),
                                &mut sidebar_painter,
                                &workspace,
                                &panes,
                                focused_pane.as_ref(),
                                zoomed.as_ref(),
                                own_client_id,
                                &agent_meta,
                            &mut vcs,
                            );
                            if chrome_changed && !overlays.is_active() {
                                repaint.raise_chrome();
                            }
                        }
                        // phux-foz.5: the `phux config reload` doorbell
                        // rang (a subscribed `phux.config.reload/v1`
                        // broadcast). Re-read our own config file and swap
                        // the config-derived state in place — same handler
                        // as the `reload-config` action; failures keep the
                        // previous config and toast.
                        if outcome.config_reload {
                            handle_config_reload(
                                out,
                                &mut keybindings_snapshot,
                                &mut resolver,
                                &mut theme,
                                &mut status_bar,
                                &mut sidebar_painter,
                                &mut plugin_actions,
                                &mut plugin_panes,
                                &mut which_key_enabled,
                                &mut which_key_delay,
                                &mut overlays,
                                &workspace,
                                &mut panes,
                                focused_pane.as_ref(),
                                zoomed.as_ref(),
                                own_client_id,
                                &agent_meta,
                                &mut vcs,
                                viewport_dims,
                                sidebar,
                                &session_name,
                            );
                        }
                        // phux-foz.7: the agent-fleet dashboard is a live
                        // projection — while it is open, a frame that
                        // changed fleet-projected state (an agent record,
                        // an ADR-0035 Asked, a pane spawn/close, a layout
                        // or session-graph change) rebuilds its rows and
                        // repaints the overlay layer. Push, not poll:
                        // nothing runs when no such frame lands.
                        //
                        // RAISED, not called: `refresh_fleet_if_open` repaints
                        // the overlay over a `paint_full_frame` base, so a call
                        // per frame is an `ESC[2J` per frame. Nine panes
                        // publishing an agent-state transition coalesce into one
                        // batch, and this arm used to fire nine times inside it —
                        // nine full-screen clears in one iteration, in exactly
                        // the view that exists for watching agents. The
                        // accumulator collapses them into ONE refresh at the
                        // drain below.
                        if fleet_dirty {
                            repaint.raise_fleet();
                        }
                        }
                        // ADR-0029 §2: the ONE drain. Every loop-level repaint
                        // trigger in this batch has raised; the highest level
                        // wins and paints exactly once. `Chrome` repaints the
                        // sidebar strip + status bar in place (no ED2, no pane
                        // re-render); `Full` clears and recomposites because
                        // the pane rects moved under us.
                        let drained = repaint.drain();
                        // The overlay half of the same drain. A no-op unless a
                        // live fleet list is actually in the overlay stack, so
                        // the raise costs nothing when the dashboard is closed.
                        if drained.fleet_dirty {
                            refresh_fleet_if_open(
                                out,
                                &mut overlays,
                                &workspace,
                                &mut panes,
                                focused_pane.as_ref(),
                                zoomed.as_ref(),
                                viewport_dims,
                                status_bar.as_mut(),
                                sidebar,
                                &mut sidebar_painter,
                                &session_name,
                                &theme,
                                &sessions,
                                focused_session,
                                &agent_meta.records,
                                &mut vcs,
                                &foreign_layouts,
                                &foreign_agents,
                            );
                        }
                        if !overlays.is_active()
                            && let Some(ls) = workspace.render_window(zoomed.as_ref()).as_deref()
                        {
                            match drained.level {
                                RepaintLevel::None => {}
                                RepaintLevel::Chrome => paint_chrome_in_place(
                                    out,
                                    ls,
                                    &panes,
                                    focused_pane.as_ref(),
                                    viewport_dims,
                                    status_bar.as_mut(),
                                    sidebar,
                                    Some(&mut sidebar_painter),
                                    &session_name,
                                ),
                                RepaintLevel::Full => paint_full_frame(
                                    out,
                                    ls,
                                    &mut panes,
                                    focused_pane.as_ref(),
                                    viewport_dims,
                                    status_bar.as_mut(),
                                    sidebar,
                                    Some(&mut sidebar_painter),
                                    &session_name,
                                ),
                            }
                        }
                    }
                    Err(AttachError::Disconnected) if detach_pending => {
                        // Server closed the socket without a `DETACHED`
                        // frame — treat it as a clean shutdown because
                        // the user requested detach. Otherwise the loop
                        // bubbles the disconnect up unchanged.
                        return Ok(LoopExit::Detached);
                    }
                    Err(err) => return Err(err),
                }
            }

            // Bound the failure mode of an application that omits `?2026l`.
            // Expose the latest complete mirror once, then let subsequent
            // output re-arm the transaction watchdog.
            () = sync_output_sleep => {
                let now = tokio::time::Instant::now();
                let mut expired = false;
                for slot in panes.values_mut() {
                    if slot.sync_output_dirty
                        && slot.sync_output_since.is_some_and(|since| {
                            now.saturating_duration_since(since) >= SYNC_OUTPUT_WATCHDOG
                        })
                    {
                        slot.sync_output_since = None;
                        slot.sync_output_dirty = false;
                        expired = true;
                    }
                }
                if expired
                    && !overlays.is_active()
                    && let Some(ls) = workspace.render_window(zoomed.as_ref()).as_deref()
                {
                    paint_full_frame(
                        out,
                        ls,
                        &mut panes,
                        focused_pane.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                    );
                }
            }

            // Bare-ESC idle timeout. Only armed when the parser has
            // pending state; resolves an ambiguous lone ESC into the
            // Escape key (see input::StdinParser::flush docs).
            () = flush_sleep => {
                let events = parser.flush();
                // phux-x2hm: a flushed bare-ESC chord can also resolve to
                // A flushed event may complete `toggle-zoom` or
                // `toggle-sidebar`; capture the old view for the same reflow
                // handshake as the stdin arm.
                let prev_zoomed = zoomed.clone();
                let prev_sidebar = sidebar;
                let prev_view_rects = view_rects(
                    &workspace,
                    prev_zoomed.as_ref(),
                    content_rect(
                        viewport_dims,
                        status_bar.as_ref().map(StatusBarPainter::position),
                        sidebar,
                    ),
                    viewport_dims,
                );
                // phux-foz.9: same agents-row snapshot as the stdin arm.
                let sidebar_agent_rows = sidebar_painter.agent_windows();
                let mut ctx = DispatchCtx {
                    resolver: resolver.as_mut(),
                    focus_history: focus_history.clone(),
                    workspace: &mut workspace,
                    viewport: viewport_dims,
                    cell_px: cell_px_dims,
                    next_request_id: &mut next_request_id,
                    pending_splits: &mut pending_splits,
                    pending_windows: &mut pending_windows,
                    overlays: &mut overlays,
                    keybindings: keybindings_snapshot.as_ref(),
                    theme: &theme,
                    sessions: &sessions,
                    foreign_layouts: &foreign_layouts,
                    foreign_agents: &foreign_agents,
                    focused_session,
                    session_name: &mut session_name,
                    switch_request: &mut switch_request,
                    zoomed: &mut zoomed,
                    sidebar,
                    sidebar_enabled: &mut sidebar_enabled,
                    sidebar_agents: &sidebar_agent_rows,
                    bar: status_bar.as_ref().map(StatusBarPainter::position),
                    status_bar: status_bar.as_ref(),
                    drag: &mut drag,
                    mouse_optout: &mut mouse_optout,
                    attention_navigation: &mut attention_navigation,
                    plugin_actions: &plugin_actions,
                    plugin_panes: &plugin_panes,
                    plugin_tx: Some(&plugin_tx),
                    reload_request: &mut reload_request,
                    agent_meta: &agent_meta.records,
                    vcs: &mut vcs,
                };
                let layout_changed = dispatch_input_events(
                    out,
                    conn,
                    events,
                    &mut focused_pane,
                    &mut detach_pending,
                    &mut predict,
                    &overlay,
                    &mut panes,
                    &mut ctx,
                )
                .await?;
                focus_history = ctx.focus_history;
                // phux-4h5a: re-fold a `toggle-sidebar` flip into the
                // reservation, same as the stdin arm, so the same-iteration
                // repaint tiles into the new content rect.
                let sidebar = sidebar_enabled.then_some(SidebarReservation {
                    edge: sidebar_edge,
                    width: sidebar_width,
                });
                // phux-eb0: same switch-on-commit check as the stdin arm.
                // A bare-ESC flush can carry the final chord of a
                // `<leader> a` selection committed via Enter.
                if let Some(target) = switch_request.take() {
                    return Ok(LoopExit::SwitchTo(target));
                }
                if zoomed != prev_zoomed || sidebar != prev_sidebar {
                    emit_view_reflow(
                        conn,
                        &workspace,
                        zoomed.as_ref(),
                        &prev_view_rects,
                        content_rect(
                        viewport_dims,
                        status_bar.as_ref().map(StatusBarPainter::position),
                        sidebar,
                    ),
                    )
                    .await?;
                }
                if layout_changed {
                    // ADR-0040: keep the agent-metadata watches in step
                    // with a pane set changed by this flush's actions.
                    sync_agent_meta_subscriptions(
                        conn,
                        panes.keys().cloned().collect(),
                        &mut agent_meta,
                        &mut next_request_id,
                    )
                    .await?;
                    refresh_window_chrome(
                        status_bar.as_mut(),
                        &mut sidebar_painter,
                        &workspace,
                        &panes,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        own_client_id,
                        &agent_meta,
                    &mut vcs,
                    );
                }
                if layout_changed
                    && !overlays.is_active()
                    && let Some(ls) = workspace.render_window(zoomed.as_ref()).as_deref()
                {
                    paint_full_frame(
                        out,
                        ls,
                        &mut panes,
                        focused_pane.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                    );
                }
                if overlays.is_active() {
                    paint_active_overlay(
                        out,
                        &overlays,
                        &workspace,
                        &mut panes,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                        &theme,
                    );
                }
                // phux-foz.5: same reload-on-commit check as the stdin
                // arm — a bare-ESC flush can carry the final chord of a
                // palette selection committing `reload-config`.
                if reload_request {
                    reload_request = false;
                    handle_config_reload(
                        out,
                        &mut keybindings_snapshot,
                        &mut resolver,
                        &mut theme,
                        &mut status_bar,
                        &mut sidebar_painter,
                        &mut plugin_actions,
                        &mut plugin_panes,
                        &mut which_key_enabled,
                        &mut which_key_delay,
                        &mut overlays,
                        &workspace,
                        &mut panes,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        own_client_id,
                        &agent_meta,
                        &mut vcs,
                        viewport_dims,
                        sidebar,
                        &session_name,
                    );
                }
            }

            // phux-foz.2: which-key idle timeout. Armed only while the
            // resolver sits at the pending-prefix state (see the update
            // above); fires once per hesitation. Pushing the popup does
            // not touch the resolver — the pending prefix stays live, so
            // the next chord executes exactly as if the popup never
            // appeared (the dispatcher's passthrough branch dismisses it
            // on the way through).
            () = which_key_sleep => {
                which_key_deadline = None;
                if push_which_key_overlay(
                    &mut overlays,
                    resolver.as_ref(),
                    keybindings_snapshot.as_ref(),
                    &theme,
                ) {
                    paint_active_overlay(
                        out,
                        &overlays,
                        &workspace,
                        &mut panes,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                        &theme,
                    );
                }
            }

            // SIGWINCH — terminal was resized. Read the new viewport
            // and ship a VIEWPORT_RESIZE upstream (SPEC §7.1 / §10.5).
            // The server uses this to recompute layout and update the
            // attached pane's dims. On query failure we fall back to a
            // sane default (logged) rather than skip the frame — the
            // server still benefits from knowing a resize happened.
            _ = sigwinch.recv() => {
                let prev_dims = viewport_dims;
                let viewport = current_viewport_or_default();
                viewport_dims = (viewport.cols.max(1), viewport.rows.max(1));
                cell_px_dims = host_cell_px(&viewport);
                // Bound predict to the FOCUSED pane's current grid, not the
                // whole viewport — predictions are pane-local (phux-7ry0). The
                // pane grids resize on the server's resize-ack snapshot, which
                // re-syncs predict again; this just keeps the transient
                // post-SIGWINCH bounds pane-shaped. Single-pane / unknown
                // falls back to the viewport.
                let (predict_cols, predict_rows) = focused_pane
                    .as_ref()
                    .and_then(|fid| panes.get(fid))
                    .map_or((viewport.cols, viewport.rows), |s| {
                        (
                            s.terminal.cols().unwrap_or(viewport.cols),
                            s.terminal.rows().unwrap_or(viewport.rows),
                        )
                    });
                predict.set_viewport(predict_cols, predict_rows);
                conn.send(&viewport_resize_frame(viewport)).await?;

                // Multi-pane: emit one TERMINAL_RESIZE per leaf whose
                // (w, h) actually changed so the server ioctls TIOCSWINSZ
                // on each PTY. Single-pane: skip the reflow math entirely
                // (no per-leaf wire emissions to make).
                if let Some(ls) = workspace.render_window(zoomed.as_ref())
                    && ls.tree.is_some()
                {
                    let bar = status_bar.as_ref().map(StatusBarPainter::position);
                    // phux-4h5a: size each PTY to the inset content rect (the
                    // pane area after the status bar + sidebar reservation),
                    // not the full viewport — otherwise an enabled sidebar
                    // resizes panes to the full width while they paint inset.
                    let prev_content = content_rect(prev_dims, bar, sidebar);
                    let new_content = content_rect(viewport_dims, bar, sidebar);
                    let prev_rects =
                        super::multi_pane::compute_layout_in(ls.as_ref(), prev_content, prev_dims)
                            .rects;
                    let diff = super::reflow::compute_reflow(
                        ls.as_ref(),
                        &prev_rects,
                        new_content,
                    );
                    if diff.too_small {
                        tracing::warn!(
                            cols = viewport_dims.0,
                            rows = viewport_dims.1,
                            "viewport too small for current layout; rendering may be garbled",
                        );
                    }
                    for (terminal_id, new_rect) in &diff.changed {
                        conn.send(&FrameKind::TerminalResize {
                            terminal_id: terminal_id.clone(),
                            cols: new_rect.w,
                            rows: new_rect.h,
                        })
                        .await?;
                    }
                }
                // phux-a7fz: do not repaint stale pre-resize mirrors into the
                // new viewport. The server resize path sends an authoritative
                // resync snapshot; painting the old grid first races with the
                // shell's prompt redraw and leaves duplicated right prompts on
                // resize-heavy shells. Clear immediately, then let the snapshot
                // repopulate the viewport at the new dimensions.
                let _ = out.write_all(b"\x1b[2J\x1b[H");
                // phux-fsb: an overlay that pinned its box to a pointer cell
                // (the context menu) is now addressing cells that may not
                // exist. Drop it BEFORE the repaint below, so this frame is
                // the one that erases it — leaving it up would keep an
                // invisible overlay capturing every keystroke, with Enter
                // committing its selected row (`Close pane`, if that is where
                // the selection sat) against a pane the user cannot see it
                // pointing at. Reflowing overlays are untouched.
                if overlays.dismiss_stale_on_resize() {
                    tracing::debug!("resize: dropped a pinned overlay whose geometry went stale");
                }
                if overlays.is_active() {
                    paint_active_overlay(
                        out,
                        &overlays,
                        &workspace,
                        &mut panes,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                        &theme,
                    );
                } else {
                    let _ = out.flush();
                }
            }

            // phux-nz4.5: periodic status-bar repaint (e.g. for the
            // `time` widget). Only fires when at least one widget has a
            // `poll_interval`. Paints in place — no pane re-render, no
            // full-screen redraw.
            () = status_tick => {
                // phux-5ke.4: an overlay above the bar would get
                // partially overwritten by the bar paint; skip ticks
                // while a modal is up.
                if !overlays.is_active() {
                    // Restore the cursor to wherever the focused pane left it
                    // so an idle tick doesn't strand the cursor in the bar.
                    let focused_cursor = focused_pane.as_ref()
                        .and_then(|fid| panes.get(fid))
                        .and_then(|slot| slot.renderer.last_cursor());
                    // phux-9xn / phux-gxy: ALWAYS provide a fallback
                    // origin. When focused_pane is None (e.g. ATTACHED
                    // hasn't seeded yet) the old code passed None →
                    // paint_bar_after_pane emitted no CUP → cursor
                    // stranded at the bar's last cell every tick.
                    let bar = status_bar.as_ref().map(StatusBarPainter::position);
                    let content = content_rect(viewport_dims, bar, sidebar);
                    let fallback_origin = Some(
                        focused_pane
                            .as_ref()
                            .and_then(|fid| {
                                workspace.render_window(zoomed.as_ref()).and_then(|ls| {
                                    super::multi_pane::compute_layout_in(
                                        ls.as_ref(),
                                        content,
                                        viewport_dims,
                                    )
                                    .rects
                                    .get(fid)
                                    .copied()
                                })
                            })
                            .map_or((0, 0), |r| (r.x, r.y)),
                    );
                    tracing::trace!(
                        focused_pane_set = focused_pane.is_some(),
                        has_cursor = focused_cursor.is_some(),
                        "status_tick: repaint bar"
                    );
                    paint_bar_after_pane(
                        status_bar.as_mut(),
                        out,
                        viewport_dims,
                        sidebar,
                        &session_name,
                        focused_cursor,
                        fallback_origin,
                        // Idle tick: nothing clobbered the bar row. The
                        // painter's content cache repaints only when a
                        // widget (e.g. the clock) actually changed.
                        false,
                    );
                }
            }

            // phux-r82.5: a spawned plugin action finished. Successes just
            // log (no modal to dismiss on the happy path); failures push a
            // dismissable toast carrying the captured output, so a broken
            // plugin is *seen* without ever having blocked the input loop.
            // The channel can't close while this loop holds `plugin_tx`,
            // so the `Some` pattern always matches when the arm fires.
            Some(result) = plugin_rx.recv() => {
                tracing::info!(
                    plugin = %result.plugin_id,
                    action = %result.action_id,
                    ok = plugin_actions::run_succeeded(&result),
                    "plugin action finished",
                );
                if let Some((title, lines)) = plugin_actions::failure_toast(&result) {
                    overlays.push(Box::new(crate::render::overlay::ToastOverlay::new(
                        title, lines, &theme,
                    )));
                    paint_active_overlay(
                        out,
                        &overlays,
                        &workspace,
                        &mut panes,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                        &theme,
                    );
                }
            }

            // SIGINT — restore the terminal explicitly (Drop wouldn't
            // fire on `exit(130)`), then exit with the shell-conventional
            // 130. `phux-roz`: this is the path that fires when the user
            // hits Ctrl-C in the outer shell after `phux attach` has
            // entered the alt screen.
            _ = sigint.recv() => {
                terminal_reset_on_signal();
                #[allow(clippy::exit, reason = "signal-driven graceful exit; Drop won't run")]
                std::process::exit(130);
            }

            // SIGTERM — `kill <pid>` from a sibling tool, supervisor, or
            // the user's tmux/screen wrapping us. Same cleanup, exit 143.
            _ = sigterm.recv() => {
                terminal_reset_on_signal();
                #[allow(clippy::exit, reason = "signal-driven graceful exit; Drop won't run")]
                std::process::exit(143);
            }

            // SIGHUP — controlling terminal went away. Restore and exit
            // 129. There is no live outer terminal to clean up, but the
            // termios restore is harmless on a dead tty and keeps the
            // cleanup path uniform.
            _ = sighup.recv() => {
                terminal_reset_on_signal();
                #[allow(clippy::exit, reason = "signal-driven graceful exit; Drop won't run")]
                std::process::exit(129);
            }
        }
    }
}

/// phux-foz.8: fetch each peer session's persisted layout — one
/// `GET_METADATA` on the per-session layout key per session other than
/// `focused` — so the window picker can render one-step cross-session
/// window rows. Correlation is via `pending` (request id -> session id);
/// replies drain through the driver's recv arm into the foreign-layout
/// cache. Best-effort: a peer with nothing persisted replies `value: None`
/// (dropped by [`apply_foreign_layout_reply`]) and keeps its fallback
/// "switch to this session" row.
async fn request_foreign_layouts(
    conn: &mut Connection,
    sessions: &[phux_protocol::wire::info::SessionInfo],
    focused: Option<phux_protocol::ids::SessionId>,
    next_request_id: &mut u32,
    pending: &mut HashMap<u32, phux_protocol::ids::SessionId>,
) -> Result<(), AttachError> {
    for s in sessions.iter().filter(|s| Some(s.id) != focused) {
        let request_id = *next_request_id;
        *next_request_id = next_request_id.wrapping_add(1);
        pending.insert(request_id, s.id);
        conn.send(&FrameKind::GetMetadata {
            request_id,
            scope: Scope::Group(DEFAULT_GROUP_ID),
            key: layout_key(s.id),
        })
        .await?;
    }
    Ok(())
}

/// phux-foz.8: fold one foreign-session layout GET reply into the picker's
/// cache. `value: None` (nothing persisted) or an undecodable envelope
/// clears the entry, so the picker falls back to the plain
/// "switch to this session" row rather than showing stale windows.
fn apply_foreign_layout_reply(
    cache: &mut HashMap<phux_protocol::ids::SessionId, Workspace>,
    session: phux_protocol::ids::SessionId,
    value: Option<&[u8]>,
) {
    match value {
        Some(bytes) => match Workspace::decode_cbor(bytes) {
            Ok(ws) => {
                cache.insert(session, ws);
            }
            Err(err) => {
                tracing::debug!(
                    session = session.get(),
                    error = %err,
                    "foreign layout decode failed; window picker keeps the fallback row",
                );
                cache.remove(&session);
            }
        },
        None => {
            cache.remove(&session);
        }
    }
}

/// phux-jpqd: fetch the `phux.agent/v1` record of every pane in one peer
/// session's just-loaded `workspace` — one `GET_METADATA` per `TerminalId`
/// leaf on the pane's agent key — so the agent-fleet dashboard's foreign
/// rows show its agent glyph/state without attaching there. Correlated
/// through `pending` (request id -> terminal id); replies fold via
/// [`apply_foreign_agent_reply`]. Skips leaves with a GET already in flight
/// so a re-fold (session-graph refresh re-requests the layout) does not
/// duplicate traffic. One-shot reads, no subscription — the same lazy-query
/// shape as [`request_foreign_layouts`] (ADR-0018 / ADR-0030).
async fn request_foreign_agents(
    conn: &mut Connection,
    workspace: &Workspace,
    next_request_id: &mut u32,
    pending: &mut HashMap<u32, TerminalId>,
) -> Result<(), AttachError> {
    // Collect the leaf ids first so the immutable borrow of `pending` (for
    // the in-flight check) is released before we mutate it in the send loop.
    let targets: Vec<TerminalId> = {
        let in_flight: std::collections::HashSet<&TerminalId> = pending.values().collect();
        let mut targets: Vec<TerminalId> = Vec::new();
        for window in &workspace.windows {
            if let Some(tree) = window.state.tree.as_ref() {
                for id in crate::layout::leaves(tree) {
                    if !in_flight.contains(&id) && !targets.contains(&id) {
                        targets.push(id);
                    }
                }
            }
        }
        targets
    };
    for id in targets {
        let request_id = *next_request_id;
        *next_request_id = next_request_id.wrapping_add(1);
        pending.insert(request_id, id.clone());
        conn.send(&FrameKind::GetMetadata {
            request_id,
            scope: Scope::Terminal(id),
            key: TERMINAL_AGENT_KEY.to_owned(),
        })
        .await?;
    }
    Ok(())
}

/// phux-jpqd: fold one foreign-pane agent-record GET reply into the fleet's
/// cache. `value: None` (no record) or an unparseable record clears the
/// entry, so the fleet row falls back to `?` / "no agent" rather than
/// showing stale identity — the same clear-on-empty policy as
/// [`apply_foreign_layout_reply`].
fn apply_foreign_agent_reply(
    cache: &mut HashMap<TerminalId, AgentRecord>,
    id: TerminalId,
    value: Option<&[u8]>,
) {
    match value.and_then(parse_agent_record) {
        Some(record) => {
            cache.insert(id, record);
        }
        None => {
            cache.remove(&id);
        }
    }
}

/// phux-jpqd: drop foreign agent records for panes no longer present in any
/// cached foreign layout (a peer closed a pane, or a session left the
/// graph), keeping the cache bounded to the live foreign pane set. Called
/// on each foreign-layout fold, before re-requesting the surviving panes.
fn prune_foreign_agents(
    cache: &mut HashMap<TerminalId, AgentRecord>,
    foreign_layouts: &HashMap<phux_protocol::ids::SessionId, Workspace>,
) {
    let live: std::collections::HashSet<TerminalId> = foreign_layouts
        .values()
        .flat_map(|ws| ws.windows.iter())
        .filter_map(|w| w.state.tree.as_ref())
        .flat_map(crate::layout::leaves)
        .collect();
    cache.retain(|id, _| live.contains(id));
}

/// phux-jpqd: rebuild and repaint the agent-fleet dashboard in place when it
/// is the active live overlay. Extracted from `main_loop`'s per-frame fleet
/// refresh so the foreign-topology intercepts (layout + agent-record GET
/// replies, which `continue` past the general frame handler) can trigger the
/// same push refresh. A no-op unless a live fleet list is on the overlay
/// stack ([`OverlayState::refresh_items`] returns `false`).
#[allow(
    clippy::too_many_arguments,
    reason = "the fleet projection reads workspace/session/agent state and the overlay repaint context — all main_loop locals threaded by reference, same shape as the paint helpers"
)]
fn refresh_fleet_if_open<W: super::RenderSink>(
    out: &mut W,
    overlays: &mut OverlayState,
    workspace: &Workspace,
    panes: &mut HashMap<TerminalId, PaneSlot>,
    focused_pane: Option<&TerminalId>,
    zoomed: Option<&TerminalId>,
    viewport_dims: (u16, u16),
    status_bar: Option<&mut StatusBarPainter>,
    sidebar: Option<SidebarReservation>,
    sidebar_painter: &mut crate::render::chrome::sidebar::SidebarPainter,
    session_name: &str,
    theme: &crate::render::Theme,
    sessions: &[phux_protocol::wire::info::SessionInfo],
    focused_session: Option<phux_protocol::ids::SessionId>,
    agent_meta: &HashMap<TerminalId, AgentRecord>,
    vcs: &mut VcsIndex,
    foreign_layouts: &HashMap<phux_protocol::ids::SessionId, Workspace>,
    foreign_agents: &HashMap<TerminalId, AgentRecord>,
) {
    if !overlays.is_active() {
        return;
    }
    let meta = super::fleet::collect_pane_meta(panes, vcs);
    let items = super::fleet::fleet_items(
        workspace,
        sessions,
        focused_session,
        agent_meta,
        &meta,
        foreign_layouts,
        foreign_agents,
    );
    if overlays.refresh_items(super::fleet::FLEET_LIVE_KEY, &items) {
        paint_active_overlay(
            out,
            overlays,
            workspace,
            panes,
            focused_pane,
            zoomed,
            viewport_dims,
            status_bar,
            sidebar,
            Some(sidebar_painter),
            session_name,
            theme,
        );
    }
}

/// Whether `key` is any session's layout key — the bare [`LAYOUT_KEY`] (legacy
/// persisted value) or a `LAYOUT_KEY/<session>` form. Used to recognise layout
/// `SET_METADATA` broadcasts (a client only ever receives its own session's).
pub(super) fn is_layout_key_string(key: &str) -> bool {
    key == LAYOUT_KEY || key.starts_with(&format!("{LAYOUT_KEY}/"))
}

/// phux-4li.5: build a [`phux_config::keybind::Resolver`] from a
/// keybindings snapshot (post phux-r82.5: the plugin-merged one, so
/// manifest `keys` chords resolve like user bindings — the merge already
/// validated each contributed chord, so a plugin can't poison this
/// build). Failures log and return `None` — a malformed `[keybindings]`
/// table degrades to "no actions are bound" rather than blocking attach.
/// Detach is a normal keybinding action, so a disabled resolver also
/// disables configured detach chords.
fn build_resolver_from(kb: &phux_config::KeybindingsCfg) -> Option<phux_config::keybind::Resolver> {
    match phux_config::keybind::Resolver::new(kb) {
        Ok(r) => Some(r),
        Err(err) => {
            tracing::warn!(error = %err, "keybind resolver build failed; disabled");
            None
        }
    }
}

/// phux-foz.2: (dis)arm the which-key popup deadline for one loop pass.
///
/// Arms (`Some(now + delay)`) only while ALL of: the resolver is pending
/// exactly at the prefix, the popup is enabled in config, and no overlay
/// is already active (a modal owns the screen; and once the popup itself
/// is up, re-arming would re-push it forever). Re-invocations while armed
/// keep the ORIGINAL deadline (anchored, like `esc_deadline`) so other
/// select! arms firing cannot postpone the popup. Any pass that sees the
/// conditions no longer met — e.g. an early continuation chord resolved
/// the prefix — disarms, which is how a fast chord suppresses the popup.
fn update_which_key_deadline(
    deadline: &mut Option<tokio::time::Instant>,
    pending_at_prefix: bool,
    enabled: bool,
    overlay_active: bool,
    now: tokio::time::Instant,
    delay: Duration,
) {
    if enabled && pending_at_prefix && !overlay_active {
        deadline.get_or_insert(now + delay);
    } else {
        *deadline = None;
    }
}

/// phux-foz.2: push the which-key popup when the timeout fires.
///
/// Re-checks the arming conditions against the CURRENT state (the select!
/// arm may race a same-iteration resolver mutation) and pushes a
/// [`WhichKeyOverlay`] built from the same keybindings snapshot the help
/// overlay uses. Returns `true` iff the popup was pushed (the caller then
/// paints the overlay layer). Never touches the resolver: the pending
/// prefix must stay live so the next chord still completes normally.
fn push_which_key_overlay(
    overlays: &mut OverlayState,
    resolver: Option<&phux_config::keybind::Resolver>,
    keybindings: Option<&phux_config::KeybindingsCfg>,
    theme: &crate::render::Theme,
) -> bool {
    if overlays.is_active() {
        return false;
    }
    if !resolver.is_some_and(phux_config::keybind::Resolver::pending_at_prefix) {
        return false;
    }
    let Some(kb) = keybindings else {
        return false;
    };
    tracing::debug!("which-key: prefix hesitation timeout; showing popup");
    overlays.push(Box::new(
        crate::render::overlay::WhichKeyOverlay::from_config(kb, theme),
    ));
    true
}

/// phux-ahv.3: snapshot the current [`Workspace`] as the `windows`
/// widget's input — display order with the active window flagged. The
/// `windows` status-bar widget formats and styles these.
/// Snapshot the window/tab strip, preferring each window's live OSC
/// title over its stored name.
///
/// A window's display label prefers, in order (ADR-0040):
///
/// 1. **The structured `phux.agent/v1` record** of the window's focused
///    leaf, when one is declared — [`AgentRecord::label`], e.g.
///    `reviewer (blocked)`. No title parsing, no substring heuristics.
/// 2. **The OSC 0/2 title** of the focused leaf — the title the running
///    program set (a shell shows the cwd/command, `vim` the file, an agent
///    its task) — read straight from that pane's client-side libghostty
///    mirror ([`PaneSlot::terminal`]). This is the tmux "automatic-rename"
///    behaviour and Warp's tab titling, entirely client-local: titles flow
///    in the PTY VT the mirror already consumes. It stays as the
///    compatibility path for agents that only speak title conventions.
/// 3. **The window's stored `name`**, when the focused leaf has no slot
///    yet or its title is empty.
fn window_infos(
    workspace: &Workspace,
    panes: &HashMap<TerminalId, PaneSlot>,
    // phux-x2hm: the driver's pane-zoom state. The active window's tab gets a
    // `Z` marker (`WindowInfo.zoomed`) when a pane is zoomed; non-active tabs
    // never show it (zoom is per the active window).
    zoomed: Option<&TerminalId>,
    // ADR-0040: Terminal → decoded `phux.agent/v1` record, kept live by the
    // driver's per-pane metadata subscriptions.
    agent_meta: &HashMap<TerminalId, AgentRecord>,
    // phux-p4vp: pane-cwd index + branch memo. The window's branch line is
    // its focused leaf's VCS branch (mut only for the memo).
    vcs: &mut VcsIndex,
) -> Vec<phux_config::widget::WindowInfo> {
    workspace
        .windows
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let focus = w.state.focus.as_ref();
            let agent_label = focus
                .and_then(|fid| agent_meta.get(fid))
                .map(AgentRecord::label);
            let title = focus
                .and_then(|fid| panes.get(fid))
                .and_then(|slot| slot.terminal.title().ok())
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(ToOwned::to_owned);
            let active = i == workspace.active;
            // phux-foz.1: a window carries attention when ANY of its leaves
            // has the ADR-0035 asked flag set — not just the focused leaf —
            // so a question in a background split still marks the tab.
            let attention = w
                .state
                .tree
                .as_ref()
                .map(crate::layout::leaves)
                .unwrap_or_default()
                .iter()
                .any(|id| panes.get(id).is_some_and(|slot| slot.attention));
            // phux-p4vp: the branch line under the label — the focused
            // leaf's cwd resolved to its VCS branch (cached file read).
            let branch = focus.and_then(|fid| vcs.branch_for_pane(fid));
            phux_config::widget::WindowInfo {
                name: agent_label.or(title).unwrap_or_else(|| w.name.clone()),
                active,
                zoomed: active && zoomed.is_some(),
                attention,
                branch,
            }
        })
        .collect()
}

/// phux-foz.9: build the sidebar's agents-section entries — one per
/// agent-running pane, every window's leaves in display order.
///
/// Identity + state per pane, in preference order:
///
/// 1. **The structured `phux.agent/v1` record** (ADR-0040), when the pane
///    declares one: name and state come straight from the record, and the
///    row carries attention when the record's effective attention is high
///    or the pane's ADR-0035 asked flag is up.
/// 2. **The OSC-title identity heuristic**
///    ([`agent_name_from_title`]) — the compatibility path for plain
///    `claude` / `codex` CLI panes, which never call `phux agent set` and
///    so never write a record. State is inferred from the only structured
///    signal the client tracks per pane: the ADR-0035 asked flag maps to
///    `blocked` (the agent is waiting on a human), otherwise `idle` — the
///    same "no blocking cue found" default `phux agent`'s detector uses
///    for a quiet screen, without scanning screen text on the render path.
///
/// A pane matching neither produces no row: the agents section lists
/// agents, not shells.
///
/// # Ordering — the attention ladder
///
/// Rows are NOT in layout order. They are sorted by
/// [`attention_rank`](crate::render::chrome::sidebar::attention_rank)
/// descending, then by most-recent state change descending (a pane that has
/// never changed sorts last), with a STABLE sort so equal-rank, equal-clock
/// rows keep window/leaf order.
///
/// This is the whole "which of my nine agents needs me?" feature. Nine panes
/// tiling a screen is nine rows the user has to read; one row pinned to the
/// top that they must act on is a glance. The rung that carries it is
/// "finished but unreviewed" outranking "still working" — a `done` agent is
/// holding a result hostage until a human reads it; a `working` agent wants
/// nothing.
fn agent_entries(
    workspace: &Workspace,
    panes: &HashMap<TerminalId, PaneSlot>,
    agent_meta: &AgentMetaIndex,
) -> Vec<AgentEntry> {
    // (entry, rank, last-change) — rank and clock drive the sort but never
    // enter `AgentEntry`, which is the sidebar painter's content-cache key.
    let mut rows: Vec<(AgentEntry, u8, Option<std::time::Instant>)> = Vec::new();
    for (i, w) in workspace.windows.iter().enumerate() {
        let leaves = w
            .state
            .tree
            .as_ref()
            .map(crate::layout::leaves)
            .unwrap_or_default();
        for id in &leaves {
            let asked = panes.get(id).is_some_and(|slot| slot.attention);
            let seen = panes.get(id).is_some_and(|slot| slot.seen);
            let change_at = agent_meta.change_at.get(id).copied();
            let mut push = |entry: AgentEntry| {
                let rank = attention_rank(entry.state, entry.attention, entry.seen);
                rows.push((entry, rank, change_at));
            };
            if let Some(record) = agent_meta.records.get(id) {
                push(AgentEntry {
                    window: i,
                    window_name: w.name.clone(),
                    name: record.name.clone(),
                    state: record.state,
                    attention: asked || record.effective_attention() == AgentAttention::High,
                    seen,
                });
                continue;
            }
            let title_name = panes
                .get(id)
                .and_then(|slot| slot.terminal.title().ok())
                .and_then(agent_name_from_title);
            if let Some(name) = title_name {
                push(AgentEntry {
                    window: i,
                    window_name: w.name.clone(),
                    name: name.to_owned(),
                    state: if asked {
                        AgentMetaState::Blocked
                    } else {
                        AgentMetaState::Idle
                    },
                    attention: asked,
                    seen,
                });
            }
        }
    }
    // Stable: rank desc, then last-change desc (`None` — never changed — sorts
    // last, since `None < Some(_)`), then declaration (window/leaf) order.
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.2.cmp(&a.2)));
    rows.into_iter().map(|(entry, _, _)| entry).collect()
}

/// Mark the focused pane as reviewed — the `seen` half of the attention ladder.
///
/// Returns `true` only on the FLIP (`false` -> `true`), so the caller can
/// schedule a chrome repaint on a real transition and nothing at all in the
/// steady state (the same shape as [`clear_attention_on_input`]).
///
/// The flip MUST be a repaint trigger. `seen` feeds both the sidebar's glyph
/// (the filled `◆` of "finished, unread" vs the quiet `○` of a reviewed row)
/// and its
/// [`attention_rank`](crate::render::chrome::sidebar::attention_rank), and the
/// focus action that made this pane focused recomputed the chrome one iteration
/// EARLIER — while the bit was still `false`. Left as a silent side effect, the
/// strip goes on claiming "finished, unreviewed", pinned above every working
/// agent, about the very pane the user is looking at, until some unrelated
/// chrome event happens to recompute [`agent_entries`].
fn mark_focused_seen(
    panes: &mut HashMap<TerminalId, PaneSlot>,
    focused_pane: Option<&TerminalId>,
) -> bool {
    focused_pane
        .and_then(|fid| panes.get_mut(fid))
        .is_some_and(|slot| !std::mem::replace(&mut slot.seen, true))
}

/// phux-foz.1: clear a pane's asked-attention flag because the user sent it
/// input (the clearing rule documented in `docs/consumers/tui.md`). Returns
/// `true` when the flag actually flipped, so the caller can schedule a
/// chrome repaint only on a real transition.
pub(super) fn clear_attention_on_input(
    panes: &mut HashMap<TerminalId, PaneSlot>,
    pane: &TerminalId,
) -> bool {
    match panes.get_mut(pane) {
        Some(slot) if slot.attention => {
            slot.attention = false;
            true
        }
        _ => false,
    }
}

/// ADR-0040 (phux-3ert): reconcile the agent-metadata index with the live
/// pane set.
///
/// For every pane that has no live `phux.agent/v1` watch yet, send a
/// one-shot `GET_METADATA` (the read-back for a record set before we
/// attached; the reply is correlated through `AgentMetaIndex::pending`) plus
/// a `SUBSCRIBE_METADATA` (the push path for later `SET`/`DELETE`
/// broadcasts). Panes that closed are pruned from every side table — the
/// server already dropped their per-Terminal store and our subscription
/// with the Terminal, so pruning is purely local hygiene. Idempotent: a
/// pane already in `subscribed` is skipped, so callers can re-run the sweep
/// on every pane-set change (bootstrap, split, new window, layout
/// broadcast) without duplicate wire traffic.
async fn sync_agent_meta_subscriptions(
    conn: &mut Connection,
    // Owned id list (not `&HashMap<_, PaneSlot>`): `PaneSlot` holds a
    // libghostty mirror that is not `Send`, and holding a reference to it
    // across the sends would make this future `!Send` (clippy
    // `future_not_send`). Callers pass `panes.keys().cloned().collect()`.
    pane_ids: Vec<TerminalId>,
    agent_meta: &mut AgentMetaIndex,
    next_request_id: &mut u32,
) -> Result<(), AttachError> {
    agent_meta.subscribed.retain(|id| pane_ids.contains(id));
    agent_meta.records.retain(|id, _| pane_ids.contains(id));
    agent_meta.pending.retain(|_, id| pane_ids.contains(id));
    // Same hygiene for the attention ladder's clock: a closed pane must not
    // leave a timestamp behind for a recycled TerminalId to inherit.
    agent_meta.change_at.retain(|id, _| pane_ids.contains(id));
    for id in &pane_ids {
        if agent_meta.subscribed.contains(id) {
            continue;
        }
        let request_id = *next_request_id;
        *next_request_id = next_request_id.wrapping_add(1);
        agent_meta.pending.insert(request_id, id.clone());
        conn.send(&FrameKind::GetMetadata {
            request_id,
            scope: Scope::Terminal(id.clone()),
            key: TERMINAL_AGENT_KEY.to_owned(),
        })
        .await?;
        conn.send(&FrameKind::SubscribeMetadata {
            scope: Scope::Terminal(id.clone()),
            key: TERMINAL_AGENT_KEY.to_owned(),
        })
        .await?;
        agent_meta.subscribed.insert(id.clone());
    }
    Ok(())
}

/// The per-leaf rect map of the zoom- and sidebar-honoring view, used as the
/// pre-toggle snapshot for the reflow handshake. Returns an empty map when
/// there is no active window or its tree is unseeded (single-pane bootstrap).
fn view_rects(
    workspace: &Workspace,
    zoomed: Option<&TerminalId>,
    content: crate::layout::Rect,
    viewport_dims: (u16, u16),
) -> HashMap<TerminalId, crate::layout::Rect> {
    workspace
        .render_window(zoomed)
        .and_then(|ls| {
            ls.tree.as_ref().map(|_| {
                super::multi_pane::compute_layout_in(ls.as_ref(), content, viewport_dims).rects
            })
        })
        .unwrap_or_default()
}

/// On a pane-zoom or sidebar toggle, emit one `TERMINAL_RESIZE` per pane whose
/// dimensions changed between the pre-toggle view and the new content view.
/// Reuses the close/SIGWINCH reflow path so each PTY's winsize tracks the
/// on-screen geometry. Sent before repainting, mirroring the other reflow sites.
async fn emit_view_reflow(
    conn: &mut Connection,
    workspace: &Workspace,
    zoomed: Option<&TerminalId>,
    prev_rects: &HashMap<TerminalId, crate::layout::Rect>,
    content: crate::layout::Rect,
) -> Result<(), AttachError> {
    let Some(ls) = workspace.render_window(zoomed) else {
        return Ok(());
    };
    if ls.tree.is_none() {
        return Ok(());
    }
    let diff = super::reflow::compute_reflow(ls.as_ref(), prev_rects, content);
    for (terminal_id, new_rect) in &diff.changed {
        conn.send(&FrameKind::TerminalResize {
            terminal_id: terminal_id.clone(),
            cols: new_rect.w,
            rows: new_rect.h,
        })
        .await?;
    }
    Ok(())
}

/// phux-nz4.5 / phux-9vf: load the on-disk config and build a
/// [`StatusBarPainter`] from `[status]`.
///
/// A malformed config never blocks attach — but it no longer vanishes
/// silently either. On a load or build failure we surface a visible
/// error line (`StatusBarPainter::error_line`) on the bar row pointing
/// the user at `phux config show` for the full diagnostic, instead of
/// dropping to an empty bar (and, alongside [`build_resolver_from`], no
/// keybindings) with only a `tracing::warn` nobody sees. Returns `None`
/// only when the config is valid and the bar would be empty (no widgets
/// configured) — callers short-circuit on that.
/// phux-foz.5: perform one explicit live config reload and repaint.
///
/// Re-runs the layered config loader ([`super::reload::reload_in_place`])
/// and, on success, swaps the driver's config-derived state — keybindings
/// snapshot, resolver, theme, status bar, plugin-action rows, which-key
/// knobs — in place, rebuilds the sidebar painter under the new theme
/// (cache-cold, so the repaint recolors everything), refreshes the window
/// chrome, and repaints. On ANY parse/validation failure the previous
/// config stays fully in effect and the error is surfaced as a
/// dismissable toast. Never crashes, never half-applies.
///
/// Reached from both reload surfaces: the `reload-config` action
/// (`DispatchCtx::reload_request`) and the `phux config reload` CLI
/// doorbell (`FrameOutcome::config_reload`).
#[allow(
    clippy::too_many_arguments,
    reason = "the config-derived slots and the repaint context are driver-loop locals threaded by reference, same shape as the paint helpers"
)]
fn handle_config_reload<W: super::RenderSink>(
    out: &mut W,
    keybindings_snapshot: &mut Option<phux_config::KeybindingsCfg>,
    resolver: &mut Option<phux_config::keybind::Resolver>,
    theme: &mut crate::render::Theme,
    status_bar: &mut Option<StatusBarPainter>,
    sidebar_painter: &mut SidebarPainter,
    plugin_actions: &mut Vec<PluginActionEntry>,
    plugin_panes: &mut Vec<plugin_panes::PluginPaneEntry>,
    which_key_enabled: &mut bool,
    which_key_delay: &mut Duration,
    overlays: &mut OverlayState,
    workspace: &Workspace,
    panes: &mut HashMap<TerminalId, PaneSlot>,
    focused_pane: Option<&TerminalId>,
    zoomed: Option<&TerminalId>,
    own_client_id: Option<ClientId>,
    agent_meta: &AgentMetaIndex,
    vcs: &mut VcsIndex,
    viewport_dims: (u16, u16),
    sidebar: Option<SidebarReservation>,
    session_name: &str,
) {
    match super::reload::reload_in_place(
        &phux_config::loader::config_path(),
        keybindings_snapshot,
        resolver,
        theme,
        status_bar,
        plugin_actions,
        plugin_panes,
        which_key_enabled,
        which_key_delay,
    ) {
        Ok(()) => {
            tracing::info!("config reloaded in place");
            // Fresh painters carry the new theme and start cache-cold so
            // the repaint below recolors the whole chrome. The attention
            // chip color rides the theme (phux-foz.1).
            *sidebar_painter = SidebarPainter::new(*theme);
            if let Some(sb) = status_bar.as_mut() {
                sb.set_attention_color(theme.attention);
            }
            refresh_window_chrome(
                status_bar.as_mut(),
                sidebar_painter,
                workspace,
                panes,
                focused_pane,
                zoomed,
                own_client_id,
                agent_meta,
                vcs,
            );
            if !overlays.is_active()
                && let Some(ls) = workspace.render_window(zoomed).as_deref()
            {
                paint_full_frame(
                    out,
                    ls,
                    panes,
                    focused_pane,
                    viewport_dims,
                    status_bar.as_mut(),
                    sidebar,
                    Some(sidebar_painter),
                    session_name,
                );
            }
        }
        Err(msg) => {
            // Keep the old config (reload_in_place touched nothing) and
            // make the failure visible: a dismissable toast, mirroring
            // the plugin-action failure surface. The status bar, theme,
            // and every binding keep working exactly as before.
            tracing::warn!(error = %msg, "config reload failed; keeping previous config");
            overlays.push(Box::new(crate::render::overlay::ToastOverlay::new(
                "Config reload failed - previous config kept",
                vec![
                    msg,
                    String::new(),
                    "Fix the file and reload again (see: phux config show)".to_owned(),
                ],
                theme,
            )));
        }
    }
    if overlays.is_active() {
        paint_active_overlay(
            out,
            overlays,
            workspace,
            panes,
            focused_pane,
            zoomed,
            viewport_dims,
            status_bar.as_mut(),
            sidebar,
            Some(&mut *sidebar_painter),
            session_name,
            theme,
        );
    }
}

fn build_status_bar_painter() -> Option<StatusBarPainter> {
    let cfg = match phux_config::loader::load() {
        Ok(c) => c,
        Err(err) => {
            tracing::warn!(error = %err, "phux-config load failed; surfacing on status bar");
            return Some(StatusBarPainter::error_line(config_error_line(&err)));
        }
    };
    let registry = phux_config::WidgetRegistry::with_builtins();
    // phux-r82.6: fold enabled plugins' `[[widgets]]` contributions in
    // after the user's own `[status]` widgets. Invalid contributions are
    // dropped with a warning inside the merge (mirroring the plugin
    // keybinding policy), so a broken plugin cannot flip the bar into the
    // error strip; a genuinely broken USER config still can, below.
    let mut status = cfg.status.clone();
    if !cfg.plugins.is_empty() {
        let config_path = phux_config::loader::config_path();
        let manifests = phux_config::plugin::load_enabled_manifests(&config_path, &cfg.plugins);
        phux_config::plugin::merge_widget_contributions(&mut status, &manifests, &registry);
    }
    match phux_config::widget::StatusBar::build(&status, &registry) {
        Ok(bar) if bar.is_empty() => None,
        Ok(bar) => {
            // phux-foz.8: `[status] position = "top" | "bottom"` picks the
            // reserved row; the pane content rect shifts to match (see
            // `paint::content_rect`).
            let mut painter = StatusBarPainter::new(bar, cfg.status.position.into());
            painter.set_prefix(cfg.keybindings.prefix);
            Some(painter)
        }
        Err(err) => {
            tracing::warn!(error = %err, "status-bar build failed; surfacing on status bar");
            Some(StatusBarPainter::error_line(config_error_line(&err)))
        }
    }
}

/// phux-9vf: format a one-line, on-screen config error for the status
/// bar. Mirrors what `phux config show` prints to stderr (the
/// `Display` of the error) and appends the actionable next step.
fn config_error_line(err: &impl std::fmt::Display) -> String {
    format!("config error: {err} (run: phux config show)")
}

/// Build a `VIEWPORT_RESIZE` frame from a [`ViewportInfo`].
///
/// Pure function, factored out of [`main_loop`] so unit tests can
/// exercise the encoder-feeding side without firing a real SIGWINCH or
/// driving a tokio runtime. The wire shape matches SPEC §7.1 / §10.5.
const fn viewport_resize_frame(viewport: ViewportInfo) -> FrameKind {
    FrameKind::ViewportResize { viewport }
}

/// Read the current viewport, falling back to 80x24 with a logged
/// warning if the kernel query fails. Used by the SIGWINCH branch
/// where we'd rather ship a stale-but-plausible viewport than skip
/// the upstream notification entirely.
fn current_viewport_or_default() -> ViewportInfo {
    match current_viewport() {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(error = %err, "tcgetwinsize failed; falling back to 80x24");
            ViewportInfo::new(80, 24)
        }
    }
}

/// Host per-cell pixel fallback when the outer terminal reports no pixel
/// geometry. MUST stay equal to the server's `DEFAULT_CELL_PX` (and the
/// kitty-graphics `FALLBACK_CELL_PX` in `paint.rs`): with no pixel report
/// the server keeps its seed cell size, and `INPUT_MOUSE` positions only
/// quantize back to the right cell if both ends assume the same geometry
/// (phux-yyex, SPEC input.md §3.1).
const HOST_CELL_PX_FALLBACK: (u16, u16) = (8, 16);

/// Derive the host's per-cell pixel size from a [`ViewportInfo`], mirroring
/// the server's SPEC L1 §9.2.1 derivation exactly (`pixel / cells`,
/// floored; degenerate axes rejected). The dispatcher scales pane-local
/// cell coordinates by this at the `INPUT_MOUSE` send boundary, so client
/// and server must floor the same division on the same numbers.
fn host_cell_px(viewport: &ViewportInfo) -> (u16, u16) {
    let derived = (|| {
        if viewport.cols == 0 || viewport.rows == 0 {
            return None;
        }
        let w = viewport.pixel_w? / viewport.cols;
        let h = viewport.pixel_h? / viewport.rows;
        (w > 0 && h > 0).then_some((w, h))
    })();
    derived.unwrap_or(HOST_CELL_PX_FALLBACK)
}

/// Read the controlling-TTY size via `tcgetwinsize` and return the
/// matching [`ViewportInfo`]. Pixel dimensions are reported when the
/// kernel provides them.
fn current_viewport() -> Result<ViewportInfo, AttachError> {
    let stdout = io::stdout();
    if !stdout.is_terminal() {
        // Fall back to a sane default if stdout isn't a TTY (rare for the
        // attach path; the early TTY check should have caught this).
        return Ok(ViewportInfo::new(80, 24));
    }
    let size = rustix::termios::tcgetwinsize(stdout.as_fd())
        .map_err(|err| AttachError::Terminal(format!("tcgetwinsize: {err}")))?;
    let pixel_w = if size.ws_xpixel == 0 {
        None
    } else {
        Some(size.ws_xpixel)
    };
    let pixel_h = if size.ws_ypixel == 0 {
        None
    } else {
        Some(size.ws_ypixel)
    };
    Ok(ViewportInfo::new(size.ws_col, size.ws_row).with_pixels(pixel_w, pixel_h))
}

/// RAII handle that flips stdin into raw mode and stdout into the alt
/// screen on construction, and restores both on drop.
///
/// Restoration runs in `Drop`, so a panic anywhere in the attach loop —
/// including the renderer or the connection — leaves the user's outer
/// terminal in a usable state.
pub struct RawModeGuard {
    original_termios: Termios,
}

impl std::fmt::Debug for RawModeGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawModeGuard").finish_non_exhaustive()
    }
}

impl RawModeGuard {
    /// Install the guard, writing the alt-screen-enter + cursor-hide
    /// sequence to real stdout. Convenience wrapper around
    /// [`Self::install_with_stdout`] for the common path; tests use
    /// the writer-injecting variant. Enables mouse capture by default
    /// (ADR-0048).
    pub fn install() -> Result<Self, AttachError> {
        Self::install_with_stdout(&mut io::stdout(), true)
    }

    /// Install the guard. Errors if stdin is not a TTY or the termios
    /// dance fails. The alt-screen + cursor-hide bytes are written to
    /// `out` so tests can capture them and assert on the regression
    /// guard for `phux-roz`.
    ///
    /// `mouse` gates the client's own outer-terminal mouse tracking
    /// (ADR-0048): when `true` the entry sequence also emits DECSET
    /// `?1002h?1006h` so divider drags work without an inner program
    /// turning mouse mode on; when `false` the client emits no mouse DECSET
    /// and only sees mouse when an inner program enables tracking (the host's
    /// native selection is untouched).
    pub fn install_with_stdout<W: Write>(out: &mut W, mouse: bool) -> Result<Self, AttachError> {
        let stdin = io::stdin();
        if !stdin.is_terminal() {
            return Err(AttachError::NotATty);
        }
        let fd = stdin.as_fd();
        let original = rustix::termios::tcgetattr(fd)
            .map_err(|err| AttachError::Terminal(format!("tcgetattr: {err}")))?;
        let mut raw = original.clone();
        raw.input_modes.remove(
            rustix::termios::InputModes::IGNBRK
                | rustix::termios::InputModes::BRKINT
                | rustix::termios::InputModes::PARMRK
                | rustix::termios::InputModes::ISTRIP
                | rustix::termios::InputModes::INLCR
                | rustix::termios::InputModes::IGNCR
                | rustix::termios::InputModes::ICRNL
                | rustix::termios::InputModes::IXON,
        );
        raw.output_modes.remove(rustix::termios::OutputModes::OPOST);
        raw.local_modes.remove(
            LocalModes::ECHO
                | LocalModes::ECHONL
                | LocalModes::ICANON
                | LocalModes::ISIG
                | LocalModes::IEXTEN,
        );
        raw.control_modes
            .remove(rustix::termios::ControlModes::CSIZE | rustix::termios::ControlModes::PARENB);
        raw.control_modes.insert(rustix::termios::ControlModes::CS8);

        // Make `read` block until at least one byte is available, with
        // no timeout. Tokio's stdin uses a blocking helper thread, so
        // this matches its expectations.
        raw.special_codes[rustix::termios::SpecialCodeIndex::VMIN] = 1;
        raw.special_codes[rustix::termios::SpecialCodeIndex::VTIME] = 0;

        rustix::termios::tcsetattr(fd, OptionalActions::Now, &raw)
            .map_err(|err| AttachError::Terminal(format!("tcsetattr: {err}")))?;

        // Enter the alt screen + hide the cursor up front so the first
        // frame paint doesn't briefly show on the normal screen. With
        // `mouse` on, also enable our own outer-terminal mouse tracking so
        // divider drags work by default (ADR-0048).
        write_enter_alt_screen(out, mouse).map_err(AttachError::Io)?;

        // Remember that we entered the alt screen so signal handlers
        // know to emit the leave sequence. We deliberately set this
        // AFTER the writes succeed so a half-completed entry doesn't
        // confuse cleanup.
        ALT_SCREEN_ACTIVE.store(true, Ordering::SeqCst);

        // Park a clone of the original Termios in process-global storage
        // so the signal-handler arms and the panic hook (which can't
        // reach the instance field) can perform a true restore rather
        // than a best-effort re-cook. The instance field remains the
        // Drop-path source of truth; the global is a snapshot.
        save_termios_snapshot(original.clone());

        Ok(Self {
            original_termios: original,
        })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        // Best-effort restore. We deliberately swallow errors — the
        // process is on its way out and a panic in Drop is worse than
        // a slightly-wedged terminal.
        //
        // Clear the global snapshot before restoring from the instance
        // field. Either source restores the same Termios (the global is
        // a clone of `original_termios`); the clear prevents a later
        // install from inheriting a stale snapshot if the next
        // `install_with_stdout` errors out before reaching the save.
        let _ = take_termios_snapshot();
        let stdin = io::stdin();
        let _ =
            rustix::termios::tcsetattr(stdin.as_fd(), OptionalActions::Now, &self.original_termios);
        let mut out = io::stdout().lock();
        let _ = write_terminal_reset(&mut out);
        ALT_SCREEN_ACTIVE.store(false, Ordering::SeqCst);
    }
}

/// Whether the alt-screen / cursor-hide sequence is currently active.
///
/// Set inside [`RawModeGuard::install_with_stdout`] after the entry
/// sequence has been emitted, cleared by [`RawModeGuard::drop`] and the
/// signal-handler cleanup. The signal path consults this so SIGINT
/// during the pre-handshake stage (no alt-screen, no raw mode) does NOT
/// emit a spurious leave sequence that the cooked terminal would print
/// as garbage.
///
/// Kept deliberately separate from [`SAVED_TERMIOS`]: alt-screen ENTER
/// and the termios flip happen at different points in
/// [`RawModeGuard::install_with_stdout`] (termios first, then alt
/// screen). Tying the two together via a single state variable would
/// couple two independent concerns and risks leaving the alt screen
/// when we should restore termios (or vice versa) on a half-failed
/// install. Two cheap flags is the right factoring.
static ALT_SCREEN_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether the client enabled its OWN outer-terminal mouse tracking
/// (DECSET `?1002h` button-motion + `?1006h` SGR) on attach (ADR-0048).
///
/// Set by [`write_enter_alt_screen`] when the `mouse` config is on, so the
/// client receives pointer reports over a divider even when the inner
/// program has no mouse mode (the common shell case) — that is what makes
/// drag-to-resize work by default. Cleared by [`write_terminal_reset`],
/// which emits the matching `?1006l?1002l` BEFORE the `?1049l` alt-screen
/// leave so the host terminal's native click-drag selection comes back on
/// detach. Kept separate from [`ALT_SCREEN_ACTIVE`] for the same reason
/// that flag is separate from the termios snapshot: independent concerns,
/// each restored exactly once.
static MOUSE_CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Snapshot of the outer terminal's pre-raw Termios, parked here so
/// the signal-handler arms in `main_loop` and the panic hook installed
/// by [`install_panic_hook_once`] can perform a true `tcsetattr`
/// restore — rather than a best-effort "force ICANON|ECHO|ISIG re-cook"
/// — when [`RawModeGuard::drop`] is unreachable (process exits via
/// `std::process::exit`, which skips Drop).
///
/// Signal-safety: the signal arms in `main_loop` are tokio
/// `signal::unix::Signal::recv()` futures, which deliver on the tokio
/// runtime thread — NOT inside a POSIX async-signal-handler context.
/// The panic hook runs on the panicking thread after unwind has begun,
/// also normal Rust context. So acquiring this `Mutex` is safe in both
/// callers; we are explicitly NOT in a context that would deadlock on
/// re-entrant lock acquisition.
///
/// Written by [`RawModeGuard::install_with_stdout`] (clone of the
/// instance's `original_termios`) and cleared by [`RawModeGuard::drop`]
/// and the signal-restore path. The instance field on `RawModeGuard`
/// remains the Drop-path source of truth; this global is a snapshot
/// for the paths that can't reach the instance.
static SAVED_TERMIOS: Mutex<Option<Termios>> = Mutex::new(None);

/// Park a Termios snapshot in [`SAVED_TERMIOS`]. Errors on lock
/// poisoning are swallowed: a poisoned lock means another thread
/// panicked while holding it, in which case we still want subsequent
/// installs to succeed and the most we lose is the signal-arm's true
/// restore (fall-back path covers it).
fn save_termios_snapshot(t: Termios) {
    if let Ok(mut slot) = SAVED_TERMIOS.lock() {
        *slot = Some(t);
    }
}

/// Take the Termios snapshot out of [`SAVED_TERMIOS`], leaving `None`.
/// Returns `None` if the lock is poisoned (signal-arm falls back to
/// the re-cook path; Drop falls back to the instance field).
fn take_termios_snapshot() -> Option<Termios> {
    SAVED_TERMIOS.lock().ok().and_then(|mut slot| slot.take())
}

/// Whether [`install_panic_hook_once`] has already run. The panic hook
/// is global to the process; we don't want a re-entrant install to
/// chain hooks indefinitely.
static PANIC_HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Write the alt-screen-enter + cursor-hide sequence, plus — when
/// `mouse` is on — the client's own mouse-tracking DECSET (ADR-0048).
/// Factored out so the install path and any future re-entry path share
/// one byte definition.
///
/// `?1002h` is button-event tracking (motion only while a button is held,
/// not `?1003h` any-motion which would flood the wire with hover traffic
/// we discard); `?1006h` is SGR extended coordinates, mandatory to address
/// columns past 223. Records [`MOUSE_CAPTURE_ACTIVE`] so the matching
/// reset emits the leave sequence.
fn write_enter_alt_screen<W: Write>(out: &mut W, mouse: bool) -> io::Result<()> {
    out.write_all(b"\x1b[?1049h")?;
    out.write_all(b"\x1b[?25l")?;
    if mouse {
        out.write_all(b"\x1b[?1002h\x1b[?1006h")?;
        MOUSE_CAPTURE_ACTIVE.store(true, Ordering::SeqCst);
    }
    out.flush()
}

/// Reconcile the client's outer-terminal mouse-tracking DECSET with
/// `want` (phux-npb3: capture follows focus).
///
/// The current state lives in [`MOUSE_CAPTURE_ACTIVE`] — the same flag
/// [`write_enter_alt_screen`] sets and [`write_terminal_reset`] consumes —
/// so a detach or signal reset while an opted-out pane holds focus never
/// emits a redundant leave sequence. No-op when the state already
/// matches; otherwise emits the ADR-0048 enter pair (`?1002h?1006h`) or
/// its reverse-order leave (`?1006l?1002l`).
/// Whether the client's outer-terminal mouse capture should currently be
/// on (phux-npb3): the global `mouse` config gate must be on AND the
/// focused pane must not have opted out via `set-pane mouse off`. With no
/// focused pane yet (pre-ATTACHED) the global gate alone decides.
fn desired_mouse_capture(
    cfg_on: bool,
    focused: Option<&TerminalId>,
    optout: &std::collections::HashSet<TerminalId>,
) -> bool {
    cfg_on && !focused.is_some_and(|id| optout.contains(id))
}

fn sync_mouse_capture<W: Write>(out: &mut W, want: bool) -> io::Result<()> {
    if MOUSE_CAPTURE_ACTIVE.swap(want, Ordering::SeqCst) == want {
        return Ok(());
    }
    if want {
        out.write_all(b"\x1b[?1002h\x1b[?1006h")?;
    } else {
        // Any-motion is a strict superset of button-event tracking; drop it
        // first so a capture-off transition while a menu is open cannot
        // leave `?1003h` armed on the host terminal.
        if HOVER_TRACKING_ACTIVE.swap(false, Ordering::SeqCst) {
            out.write_all(b"\x1b[?1003l")?;
        }
        out.write_all(b"\x1b[?1006l\x1b[?1002l")?;
    }
    out.flush()
}

/// Whether the client upgraded the outer terminal to any-motion reporting
/// (`?1003h`) on top of its button-event capture (phux-wrnm).
///
/// ADR-0048 deliberately enables only `?1002h`: hover traffic the client
/// discards is wasted bytes on every pointer move. A context menu is the
/// one thing that *does* consume it — it hover-tracks the row under the
/// pointer with no button held — so the mode is raised while such an
/// overlay is on the stack and dropped the moment it closes. Kept as its
/// own flag (not folded into [`MOUSE_CAPTURE_ACTIVE`]) so the capture
/// reconcile and the reset path each restore exactly what they set.
static HOVER_TRACKING_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Reconcile any-motion reporting with `want` (phux-wrnm).
///
/// A no-op unless the state changed, like [`sync_mouse_capture`]. Never
/// raises `?1003h` while capture is off: with no capture the client has no
/// business reporting motion at all, and the host terminal owns the mouse.
/// Leaving hover mode re-asserts `?1002h` — some terminals treat the two
/// DECSETs as one tracking mode and `?1003l` alone would drop button
/// reporting with it, killing divider drags.
fn sync_hover_tracking<W: Write>(out: &mut W, want: bool) -> io::Result<()> {
    let want = want && MOUSE_CAPTURE_ACTIVE.load(Ordering::SeqCst);
    if HOVER_TRACKING_ACTIVE.swap(want, Ordering::SeqCst) == want {
        return Ok(());
    }
    if want {
        out.write_all(b"\x1b[?1003h")?;
    } else {
        out.write_all(b"\x1b[?1003l\x1b[?1002h")?;
    }
    out.flush()
}

/// Restore the outer terminal to a sane post-attach state: drop SGR,
/// show the cursor, and (if we ever entered the alt screen) leave it.
///
/// Used by both [`RawModeGuard::drop`] and the signal-handler arms in
/// the private `main_loop` function. Safe to call multiple times — the
/// second call sees
/// `ALT_SCREEN_ACTIVE == false` and skips the leave sequence.
pub fn write_terminal_reset<W: Write>(out: &mut W) -> io::Result<()> {
    write_reset(out)?;
    // phux-wrnm: a context menu open at detach (or at SIGINT) left the
    // terminal in any-motion mode; drop that before the capture pair so the
    // host is handed back exactly what it had.
    if HOVER_TRACKING_ACTIVE.swap(false, Ordering::SeqCst) {
        out.write_all(b"\x1b[?1003l")?;
        out.flush()?;
    }
    // ADR-0048: drop our own mouse tracking BEFORE leaving the alt screen,
    // so the host terminal's native click-drag selection is restored on
    // detach. `?1006l` then `?1002l` undoes the entry pair in reverse.
    if MOUSE_CAPTURE_ACTIVE.swap(false, Ordering::SeqCst) {
        out.write_all(b"\x1b[?1006l\x1b[?1002l")?;
        out.flush()?;
    }
    if ALT_SCREEN_ACTIVE.swap(false, Ordering::SeqCst) {
        out.write_all(b"\x1b[?1049l")?;
        out.flush()?;
    }
    Ok(())
}

/// Best-effort terminal reset from inside a signal handler arm. This
/// is the SIGINT/SIGTERM/SIGHUP path: termios goes back to the saved
/// state (recovered from [`SAVED_TERMIOS`] when populated; otherwise a
/// re-cook fall-back), and the alt-screen sequence is left if we
/// entered one. Errors are swallowed — the process is on its way out.
///
/// Behaviour change for phux-2r7 (was best-effort re-cook only,
/// committed in 63dc6ff): when [`RawModeGuard`] has parked a snapshot,
/// we now do a true `tcsetattr` restore to the user's pre-attach
/// flags, preserving customisations like IUTF8 / VEOF that the re-cook
/// would clobber. The manual SIGINT-during-attach repro that motivated
/// the original fix still passes; verifying the precise-restore
/// behaviour requires a live PTY and is not unit-testable from here.
fn terminal_reset_on_signal() {
    let stdin = io::stdin();
    let fd = stdin.as_fd();
    if let Some(saved) = take_termios_snapshot() {
        // True restore: the snapshot is exactly what `tcgetattr`
        // returned before we flipped into raw mode.
        let _ = rustix::termios::tcsetattr(fd, OptionalActions::Now, &saved);
    } else if let Ok(mut termios) = rustix::termios::tcgetattr(fd) {
        // Fall-back re-cook for the (rare) case where the snapshot is
        // missing — e.g. signal fired before `install_with_stdout`
        // reached the save, or the lock was poisoned. We force the
        // canonical-mode flags back on so the cooked shell at least
        // shows what the user types; non-default flags are NOT
        // preserved on this path and the user may want to run `reset`
        // after.
        termios.local_modes.insert(
            LocalModes::ECHO
                | LocalModes::ECHONL
                | LocalModes::ICANON
                | LocalModes::ISIG
                | LocalModes::IEXTEN,
        );
        termios.input_modes.insert(
            rustix::termios::InputModes::BRKINT
                | rustix::termios::InputModes::ICRNL
                | rustix::termios::InputModes::IXON,
        );
        termios
            .output_modes
            .insert(rustix::termios::OutputModes::OPOST);
        let _ = rustix::termios::tcsetattr(fd, OptionalActions::Now, &termios);
    }
    let mut out = io::stdout().lock();
    let _ = write_terminal_reset(&mut out);
}

/// Clean client exit after a server-acknowledged DETACH (or a
/// detach-intended disconnect). Restores the terminal and exits the
/// process immediately rather than returning up the stack.
///
/// Why not just `return Ok(())` and let `RawModeGuard::drop` + the
/// runtime teardown clean up? Because `tokio::io::stdin()` parks an
/// **uncancellable** blocking `read()` on a helper thread. The terminal
/// restore (guard Drop) does run, but the subsequent runtime drop then
/// blocks forever waiting for that stuck read to return. The result is
/// a zombie client that never exits, keeps a reader on the shared PTY,
/// and steals the first line the user types next — most painfully their
/// reattach command, which is why reattach "did nothing." Exiting here
/// closes that window: the restore mirrors the signal path, and
/// `process::exit` skips the teardown that would otherwise hang.
#[allow(
    clippy::exit,
    reason = "detach must exit now; runtime drop hangs on the stdin read thread"
)]
fn exit_after_detach() -> ! {
    terminal_reset_on_signal();
    std::process::exit(0);
}

/// Install a global panic hook that first records the panic to the
/// `tracing` file sink, then runs [`write_terminal_reset`], then chains
/// the previous (default) hook. Idempotent — repeated calls after the
/// first are no-ops.
///
/// Ordering matters and is deliberate:
///
/// 1. **Log first.** The client's `tracing` subscriber writes to a file
///    (never stderr — the alt screen is up), so we emit the panic message
///    plus a captured [`std::backtrace::Backtrace`] there BEFORE touching
///    the terminal. This is the durable record: even though the next step
///    restores the cooked terminal and the default hook's stderr backtrace
///    lands on a screen the user may not be watching, the crash is fully
///    recoverable from the log file.
/// 2. **Restore the terminal.** Without this, a panic deep inside the
///    renderer or libghostty would unwind through `main_loop` and the
///    default hook would print into the alt screen we're about to leave —
///    so the user would see nothing.
/// 3. **Chain the previous hook** (the default backtrace printer).
fn install_panic_hook_once() {
    if PANIC_HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // (1) Durable capture to the file sink before the terminal is
        // touched. `Backtrace::capture` honors `RUST_BACKTRACE`: it is
        // `Disabled` (rendered as a hint) unless the env var is set, so
        // there's no symbolication cost in the common case while a full
        // trace is available when the operator asks for one.
        let backtrace = std::backtrace::Backtrace::capture();
        let location = info
            .location()
            .map_or_else(|| "<unknown>".to_owned(), ToString::to_string);
        tracing::error!(
            panic.location = %location,
            panic.message = %info,
            panic.backtrace = %backtrace,
            "client panic",
        );
        // (2) Restore the outer terminal so the chained hook's output
        // doesn't vanish into the dead alt screen.
        terminal_reset_on_signal();
        // (3) Default hook: prints the panic + backtrace to stderr.
        previous(info);
    }));
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::testkit::{ScriptSpec, ScriptedServer};
    use phux_protocol::caps::{TerminalColor, TerminalDefaultColors};
    use tokio::net::UnixStream;

    static TERMINAL_RESET_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// phux-0db: a session created from inside the TUI (picker "new
    /// session") seeds its pane in the client's cwd, not `None` (= the
    /// daemon's CWD).
    #[test]
    fn create_session_target_carries_client_cwd() {
        let expected = std::env::current_dir()
            .expect("test cwd")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            create_session_target("picker".to_owned()),
            AttachTarget::CreateIfMissing {
                name: "picker".to_owned(),
                command: None,
                cwd: Some(expected),
            }
        );
    }

    #[test]
    fn sidebar_reservation_changes_view_rects_for_pty_reflow() {
        let id = TerminalId::local(1);
        let workspace = Workspace::single(id.clone());
        let viewport = (100, 30);
        let full = view_rects(
            &workspace,
            None,
            content_rect(viewport, None, None),
            viewport,
        );
        let inset = view_rects(
            &workspace,
            None,
            content_rect(
                viewport,
                None,
                Some(SidebarReservation {
                    edge: SidebarEdge::Left,
                    width: 20,
                }),
            ),
            viewport,
        );

        assert_eq!(full.get(&id).expect("full rect").w, 100);
        assert_eq!(inset.get(&id).expect("inset rect").w, 80);
        assert_eq!(inset.get(&id).expect("inset rect").x, 20);
    }

    #[test]
    fn pane_slot_initializes_nonzero_cell_pixels_for_live_kitty_render() {
        let mut slot = PaneSlot::new_with_size(10, 5).expect("slot");
        slot.terminal
            .vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=77,q=2;/wAA/w==\x1b\\");

        let mut out = Vec::new();
        slot.renderer
            .render(&slot.terminal, &mut out)
            .expect("render");
        let replay = String::from_utf8_lossy(&out);
        assert!(
            replay.contains("\x1b_Ga=T,f=32,s=1,v=1,i=77,q=2,c=1,r=1,m=0;/wAA/w==\x1b\\"),
            "initial live render must replay classic Kitty placement; got {replay:?}"
        );
    }

    #[test]
    fn supervisory_badge_formats_every_state() {
        // ADR-0033: the focused-pane supervisory badge. Running + un-leased
        // shows nothing; frozen and lease-holder render distinct chips, and the
        // holder is "you" only when it matches this client's own id.
        let me = ClientId::new(7);
        let other = ClientId::new(9);
        assert_eq!(format_supervisory_badge(false, None, Some(me)), None);
        assert_eq!(
            format_supervisory_badge(true, None, Some(me)).as_deref(),
            Some("[ FROZEN ]")
        );
        assert_eq!(
            format_supervisory_badge(false, Some(me), Some(me)).as_deref(),
            Some("[ WHEEL:you ]")
        );
        assert_eq!(
            format_supervisory_badge(false, Some(other), Some(me)).as_deref(),
            Some("[ WHEEL:c9 ]")
        );
        assert_eq!(
            format_supervisory_badge(true, Some(other), Some(me)).as_deref(),
            Some("[ FROZEN WHEEL:c9 ]")
        );
        // No own id yet (pre-ATTACHED): a holder still renders by id, never "you".
        assert_eq!(
            format_supervisory_badge(false, Some(me), None).as_deref(),
            Some("[ WHEEL:c7 ]")
        );
    }

    /// phux-foz.1: the status-bar attention hint. Nothing asking shows
    /// nothing; one asking pane shows the plain chip; several asking panes
    /// carry the count.
    #[test]
    fn attention_hint_formats_every_count() {
        assert_eq!(format_attention_hint(0), None);
        assert_eq!(format_attention_hint(1).as_deref(), Some("[ ASK ]"));
        assert_eq!(format_attention_hint(3).as_deref(), Some("[ ASK x3 ]"));
    }

    /// phux-oih5.16: the driver holds exactly one client-local origin. A
    /// second attention jump cannot overwrite it, and return consumes it.
    #[test]
    fn attention_navigation_saves_once_and_consumes() {
        let mut navigation = AttentionNavigation::default();
        navigation.save_origin_once(Some(&TerminalId::local(1)));
        navigation.save_origin_once(Some(&TerminalId::local(2)));
        assert_eq!(navigation.take_origin(), Some(TerminalId::local(1)));
        assert_eq!(navigation.take_origin(), None);
    }

    /// phux-foz.1: key/paste input forwarded to a pane clears its asked
    /// flag exactly once — the transition reports `true`, repeats and
    /// unknown panes report `false` (no spurious chrome repaints).
    #[test]
    fn clear_attention_on_input_clears_once() {
        let id = TerminalId::local(1);
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        let mut slot = PaneSlot::new_with_size(80, 24).expect("slot");
        slot.attention = true;
        panes.insert(id.clone(), slot);

        assert!(clear_attention_on_input(&mut panes, &id), "first clear");
        assert!(
            !panes.get(&id).expect("slot").attention,
            "flag must be down after the clear"
        );
        assert!(
            !clear_attention_on_input(&mut panes, &id),
            "already-clear pane reports no transition"
        );
        assert!(
            !clear_attention_on_input(&mut panes, &TerminalId::local(9)),
            "unknown pane reports no transition"
        );
    }

    /// phux-foz.1: `window_infos` marks a window when ANY of its leaves has
    /// the asked flag — including a non-focused leaf — and only that window.
    #[test]
    fn window_infos_flags_attention_on_the_asking_window() {
        let front = TerminalId::local(1);
        let back = TerminalId::local(2);
        let mut workspace = Workspace::single(front.clone());
        workspace.add_window("2".to_owned(), back.clone());
        workspace.select(0);
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        panes.insert(front, PaneSlot::new_with_size(80, 24).expect("slot"));
        let mut asking = PaneSlot::new_with_size(80, 24).expect("slot");
        asking.attention = true;
        panes.insert(back.clone(), asking);

        let infos = window_infos(
            &workspace,
            &panes,
            None,
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        assert!(
            !infos[0].attention,
            "quiet window must not carry the marker"
        );
        assert!(
            infos[1].attention,
            "the asking (background) window carries the marker"
        );

        // Clearing the flag clears the marker.
        assert!(clear_attention_on_input(&mut panes, &back));
        let infos = window_infos(
            &workspace,
            &panes,
            None,
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        assert!(!infos[1].attention);
    }

    #[test]
    fn attach_error_io_display_includes_source() {
        let err = AttachError::Io(io::Error::other("boom"));
        let msg = err.to_string();
        assert!(msg.contains("attach loop io error"));
    }

    // -- which-key popup arming (phux-foz.2) ------------------------------

    /// Build a resolver from the shipped defaults and walk it to the
    /// pending-prefix state (`C-a` fed, continuation awaited).
    fn pending_resolver() -> phux_config::keybind::Resolver {
        let cfg =
            phux_config::parse_str(phux_config::DEFAULT_CONFIG_TOML, Path::new("default.toml"))
                .expect("default config parses");
        let mut r = phux_config::keybind::Resolver::new(&cfg.keybindings).expect("resolver builds");
        let prefix = phux_config::keybind::parse_chord(&cfg.keybindings.prefix).expect("prefix");
        assert_eq!(r.feed(prefix), phux_config::keybind::Feed::Partial);
        assert!(r.pending_at_prefix());
        r
    }

    #[test]
    fn which_key_deadline_arms_once_and_holds_its_anchor() {
        let mut deadline = None;
        let now = tokio::time::Instant::now();
        let delay = Duration::from_millis(600);
        update_which_key_deadline(&mut deadline, true, true, false, now, delay);
        assert_eq!(deadline, Some(now + delay), "arms at now + delay");
        // A later pass (other select! arms fired) keeps the ORIGINAL
        // anchor — the popup is not postponed by unrelated wakeups.
        update_which_key_deadline(
            &mut deadline,
            true,
            true,
            false,
            now + Duration::from_millis(300),
            delay,
        );
        assert_eq!(deadline, Some(now + delay), "anchor survives re-passes");
    }

    #[test]
    fn which_key_deadline_disarms_when_an_early_chord_resolves() {
        // The suppression path: prefix pressed (armed), then a fast
        // continuation resolves the chord BEFORE the timeout — the next
        // loop pass sees pending=false and must disarm, so the popup
        // never appears.
        let mut deadline = None;
        let now = tokio::time::Instant::now();
        let delay = Duration::from_millis(600);
        update_which_key_deadline(&mut deadline, true, true, false, now, delay);
        assert!(deadline.is_some());
        update_which_key_deadline(&mut deadline, false, true, false, now, delay);
        assert_eq!(deadline, None, "early chord suppresses the popup");
    }

    #[test]
    fn which_key_deadline_respects_disable_and_active_overlay() {
        let mut deadline = None;
        let now = tokio::time::Instant::now();
        let delay = Duration::from_millis(600);
        // Disabled in config: never arms.
        update_which_key_deadline(&mut deadline, true, false, false, now, delay);
        assert_eq!(deadline, None);
        // A modal already up: never arms (it owns input; the resolver was
        // reset on entry anyway).
        update_which_key_deadline(&mut deadline, true, true, true, now, delay);
        assert_eq!(deadline, None);
        // Armed, then an overlay appears before the timeout: disarms.
        update_which_key_deadline(&mut deadline, true, true, false, now, delay);
        assert!(deadline.is_some());
        update_which_key_deadline(&mut deadline, true, true, true, now, delay);
        assert_eq!(deadline, None);
    }

    #[test]
    fn which_key_timeout_pushes_the_popup_and_keeps_the_prefix_pending() {
        // The timeout path: a pending-at-prefix resolver + keybindings
        // snapshot ⇒ the popup is pushed; the resolver still holds the
        // pending prefix so the NEXT chord completes normally.
        let cfg =
            phux_config::parse_str(phux_config::DEFAULT_CONFIG_TOML, Path::new("default.toml"))
                .expect("default config parses");
        let resolver = pending_resolver();
        let mut overlays = OverlayState::new();
        let theme = crate::render::Theme::default();
        let pushed = push_which_key_overlay(
            &mut overlays,
            Some(&resolver),
            Some(&cfg.keybindings),
            &theme,
        );
        assert!(pushed, "timeout must push the which-key popup");
        assert!(overlays.is_active());
        assert!(
            overlays.top_is_passthrough(),
            "the popup must be input-passthrough so it can never eat a chord"
        );
        assert!(
            resolver.pending_at_prefix(),
            "pushing the popup must not consume the pending prefix"
        );
    }

    #[test]
    fn which_key_push_declines_without_pending_prefix_or_over_a_modal() {
        let cfg =
            phux_config::parse_str(phux_config::DEFAULT_CONFIG_TOML, Path::new("default.toml"))
                .expect("default config parses");
        let theme = crate::render::Theme::default();

        // Resolver at the root (no pending prefix): no push.
        let idle = phux_config::keybind::Resolver::new(&cfg.keybindings).expect("resolver builds");
        let mut overlays = OverlayState::new();
        assert!(!push_which_key_overlay(
            &mut overlays,
            Some(&idle),
            Some(&cfg.keybindings),
            &theme,
        ));
        assert!(!overlays.is_active());

        // A modal already up: no push (would stack over user input).
        let pending = pending_resolver();
        let mut overlays = OverlayState::new();
        overlays.push(Box::new(crate::render::overlay::HelpOverlay::from_config(
            &cfg.keybindings,
            &theme,
        )));
        assert!(!push_which_key_overlay(
            &mut overlays,
            Some(&pending),
            Some(&cfg.keybindings),
            &theme,
        ));
        assert_eq!(overlays.depth(), 1, "nothing stacked on the modal");
    }

    /// phux-jy4t: the layout metadata key is per-session, so two sessions
    /// never share (and clobber) one bucket.
    #[test]
    fn layout_key_is_per_session() {
        use phux_protocol::ids::SessionId;
        let a = layout_key(SessionId::new(1));
        let b = layout_key(SessionId::new(2));
        assert_eq!(a, "phux.tui.layout/v1/1");
        assert_eq!(b, "phux.tui.layout/v1/2");
        assert_ne!(a, b, "different sessions get different keys");
        assert!(a.starts_with(LAYOUT_KEY), "still under the layout prefix");
    }

    /// phux-foz.8: a foreign session's layout GET reply round-trips into
    /// the picker cache; a tombstone (`None`) or garbage clears/skips the
    /// entry so the picker falls back to the plain switch row.
    #[test]
    fn apply_foreign_layout_reply_caches_clears_and_survives_garbage() {
        use phux_protocol::ids::SessionId;
        let sid = SessionId::new(7);
        let mut cache: HashMap<SessionId, Workspace> = HashMap::new();

        // A decodable envelope lands in the cache with its windows intact.
        let mut ws = Workspace::single(TerminalId::local(1));
        ws.add_window("logs".to_owned(), TerminalId::local(2));
        let bytes = ws.encode_cbor().expect("encode");
        apply_foreign_layout_reply(&mut cache, sid, Some(&bytes));
        assert_eq!(cache.get(&sid).map(|w| w.windows.len()), Some(2));

        // Garbage clears the stale entry rather than keeping it.
        apply_foreign_layout_reply(&mut cache, sid, Some(b"not cbor"));
        assert!(!cache.contains_key(&sid), "undecodable reply clears");

        // Re-cache, then a tombstone (nothing persisted) clears again.
        apply_foreign_layout_reply(&mut cache, sid, Some(&bytes));
        assert!(cache.contains_key(&sid));
        apply_foreign_layout_reply(&mut cache, sid, None);
        assert!(!cache.contains_key(&sid), "tombstone clears");
    }

    /// phux-jpqd: a foreign pane's agent-record GET reply round-trips into
    /// the fleet cache; a tombstone (`None`) or an unparseable record
    /// clears the entry so the fleet row falls back to `?`/"no agent".
    #[test]
    fn apply_foreign_agent_reply_caches_clears_and_survives_garbage() {
        let id = TerminalId::local(3);
        let mut cache: HashMap<TerminalId, AgentRecord> = HashMap::new();

        // A well-formed record lands with its identity intact.
        let record = AgentRecord {
            name: "packer".to_owned(),
            kind: Some("codex".to_owned()),
            state: AgentMetaState::Working,
            ..AgentRecord::default()
        };
        apply_foreign_agent_reply(&mut cache, id.clone(), Some(&record.encode()));
        assert_eq!(cache.get(&id).map(|r| r.name.as_str()), Some("packer"));

        // Garbage (no non-empty `name`) clears the stale entry.
        apply_foreign_agent_reply(&mut cache, id.clone(), Some(b"not json"));
        assert!(!cache.contains_key(&id), "unparseable record clears");

        // Re-cache, then a tombstone (no record) clears again.
        apply_foreign_agent_reply(&mut cache, id.clone(), Some(&record.encode()));
        assert!(cache.contains_key(&id));
        apply_foreign_agent_reply(&mut cache, id.clone(), None);
        assert!(!cache.contains_key(&id), "tombstone clears");
    }

    /// phux-jpqd: pruning keeps only the agent records whose panes still
    /// appear in some cached foreign layout — a peer closing a pane (or a
    /// session leaving the graph) evicts its record so the cache stays
    /// bounded to the live foreign pane set.
    #[test]
    fn prune_foreign_agents_retains_only_live_foreign_panes() {
        use phux_protocol::ids::SessionId;
        let live = TerminalId::local(1);
        let stale = TerminalId::local(2);
        let mut cache: HashMap<TerminalId, AgentRecord> = HashMap::new();
        cache.insert(live.clone(), AgentRecord::default());
        cache.insert(stale.clone(), AgentRecord::default());

        // One foreign layout holds only `live`.
        let mut foreign_layouts: HashMap<SessionId, Workspace> = HashMap::new();
        foreign_layouts.insert(SessionId::new(9), Workspace::single(live.clone()));

        prune_foreign_agents(&mut cache, &foreign_layouts);
        assert!(
            cache.contains_key(&live),
            "a pane still in a layout survives"
        );
        assert!(
            !cache.contains_key(&stale),
            "a pane in no layout is evicted"
        );

        // No cached layouts at all evicts everything.
        prune_foreign_agents(&mut cache, &HashMap::new());
        assert!(cache.is_empty(), "no foreign layouts => no foreign agents");
    }

    #[test]
    fn is_layout_key_string_matches_the_family_only() {
        // Bare legacy key + any session-suffixed key are layout keys.
        assert!(is_layout_key_string(LAYOUT_KEY));
        assert!(is_layout_key_string("phux.tui.layout/v1/7"));
        // A different key that merely shares the prefix-without-separator is
        // NOT matched, and unrelated keys aren't either.
        assert!(!is_layout_key_string("phux.tui.layout/v12"));
        assert!(!is_layout_key_string("phux.tui.other/v1"));
    }

    #[test]
    fn coalesce_defers_every_pane_frame_but_its_last() {
        // phux-jhv8: in a coalesced burst, every output frame for a pane
        // defers EXCEPT that pane's final frame, which settles the screen.
        let p = |id| Some(TerminalId::Local { id });
        // Single-pane burst: only the last frame paints.
        assert_eq!(
            coalesce_defer_flags(&[p(2), p(2), p(2)]),
            vec![true, true, false]
        );
        // A lone frame never defers (preserves the one-frame-one-paint path).
        assert_eq!(coalesce_defer_flags(&[p(2)]), vec![false]);
    }

    #[test]
    fn coalesce_keys_deferral_per_pane_not_globally() {
        // Two panes interleaved: each pane's LAST frame paints, so neither is
        // left stale even when the burst ends on the other pane's output.
        let p = |id| Some(TerminalId::Local { id });
        // A(defer, later A) B(defer, later B) A(last A) B(last B)
        assert_eq!(
            coalesce_defer_flags(&[p(1), p(2), p(1), p(2)]),
            vec![true, true, false, false]
        );
        // Burst ending on a non-focused pane B must still paint A's last frame.
        assert_eq!(
            coalesce_defer_flags(&[p(1), p(1), p(2)]),
            vec![true, false, false]
        );
    }

    #[test]
    fn snapshot_never_defers_even_behind_a_later_same_pane_frame() {
        // A snapshot is authoritative full state: even when the coalesce mask
        // says "defer" (a later same-pane output exists in the burst), it must
        // still paint, or the later incremental output paints onto a screen the
        // snapshot never drew — the attach/reattach/split "mangled" bug.
        let snap = FrameKind::TerminalSnapshot {
            terminal_id: TerminalId::Local { id: 1 },
            cols: 80,
            rows: 24,
            vt_replay_bytes: Vec::new(),
            scrollback_bytes: None,
        };
        let output = FrameKind::TerminalOutput {
            terminal_id: TerminalId::Local { id: 1 },
            seq: 1,
            bytes: Vec::new().into(),
        };
        // Mask says defer, but a snapshot overrides it and paints.
        assert!(!frame_defers_paint(true, &snap));
        // An ordinary output still honors the coalesce mask.
        assert!(frame_defers_paint(true, &output));
        assert!(!frame_defers_paint(false, &output));
    }

    #[test]
    fn coalesce_control_frames_never_defer() {
        // `None` (a non-painting control frame) never defers, and never
        // counts as a later same-pane paint for the frames before it.
        let p = |id| Some(TerminalId::Local { id });
        assert_eq!(
            coalesce_defer_flags(&[p(1), None, p(1)]),
            vec![true, false, false]
        );
        assert_eq!(coalesce_defer_flags(&[None, None]), vec![false, false]);
        assert_eq!(coalesce_defer_flags(&[]), Vec::<bool>::new());
    }

    #[test]
    fn window_infos_prefers_osc_title_over_stored_name() {
        // A program in the focused leaf sets an OSC 2 window title; the tab
        // strip must show it (tmux automatic-rename / Warp tab titling).
        let id = TerminalId::local(1);
        let workspace = Workspace::single(id.clone());
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        let mut slot = PaneSlot::new_with_size(80, 24).expect("slot");
        slot.terminal.vt_write(b"\x1b]2;~/src/phux\x07");
        panes.insert(id, slot);

        let infos = window_infos(
            &workspace,
            &panes,
            None,
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        assert_eq!(infos.len(), 1);
        assert_eq!(
            infos[0].name, "~/src/phux",
            "the OSC title should label the tab, overriding the stored name"
        );
        assert!(infos[0].active);
    }

    #[test]
    fn window_infos_falls_back_to_stored_name_without_title() {
        // No OSC title set ⇒ the window's stored name ("1" for the first).
        let id = TerminalId::local(1);
        let workspace = Workspace::single(id.clone());
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        panes.insert(id, PaneSlot::new_with_size(80, 24).expect("slot"));

        let infos = window_infos(
            &workspace,
            &panes,
            None,
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        assert_eq!(infos[0].name, "1");
    }

    #[test]
    fn window_infos_ignores_a_whitespace_only_title() {
        // A title of only spaces is not a useful label; fall back to the name.
        let id = TerminalId::local(1);
        let workspace = Workspace::single(id.clone());
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        let mut slot = PaneSlot::new_with_size(80, 24).expect("slot");
        slot.terminal.vt_write(b"\x1b]2;   \x07");
        panes.insert(id, slot);

        let infos = window_infos(
            &workspace,
            &panes,
            None,
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        assert_eq!(infos[0].name, "1");
    }

    #[test]
    fn window_infos_prefers_agent_record_over_osc_title() {
        // ADR-0040: a declared `phux.agent/v1` record labels the window from
        // structured data — the OSC title (set here to an unrelated string)
        // must NOT leak through, and no substring parsing is involved.
        let id = TerminalId::local(1);
        let workspace = Workspace::single(id.clone());
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        let mut slot = PaneSlot::new_with_size(80, 24).expect("slot");
        slot.terminal.vt_write(b"\x1b]2;~/src/phux\x07");
        panes.insert(id.clone(), slot);
        let mut records: HashMap<TerminalId, AgentRecord> = HashMap::new();
        records.insert(
            id,
            AgentRecord {
                name: "reviewer".to_owned(),
                state: crate::agent_meta::AgentMetaState::Blocked,
                ..AgentRecord::default()
            },
        );

        let infos = window_infos(&workspace, &panes, None, &records, &mut VcsIndex::default());
        assert_eq!(
            infos[0].name, "!reviewer (blocked)",
            "structured record must beat the OSC title"
        );
    }

    #[test]
    fn window_infos_falls_back_to_title_when_record_cleared() {
        // ADR-0040 compatibility path: no record ⇒ the OSC title labels the
        // tab exactly as before.
        let id = TerminalId::local(1);
        let workspace = Workspace::single(id.clone());
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        let mut slot = PaneSlot::new_with_size(80, 24).expect("slot");
        slot.terminal.vt_write(b"\x1b]2;claude task\x07");
        panes.insert(id, slot);

        let infos = window_infos(
            &workspace,
            &panes,
            None,
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        assert_eq!(infos[0].name, "claude task");
    }

    /// An `AgentMetaIndex` holding `records` and nothing else — the shape
    /// `agent_entries` reads.
    fn meta_index(records: HashMap<TerminalId, AgentRecord>) -> AgentMetaIndex {
        AgentMetaIndex {
            records,
            ..AgentMetaIndex::default()
        }
    }

    /// The attention ladder, end to end through `agent_entries`: an UNSEEN
    /// `done` agent must sort ABOVE a `working` one, and a `blocked` one above
    /// both. This is the "which of my agents needs me?" contract — a finished
    /// agent is holding a result hostage until a human reads it, so it must
    /// outrank one that is merely still busy.
    #[test]
    fn agent_entries_rank_unreviewed_done_above_working() {
        let working = TerminalId::local(1);
        let done = TerminalId::local(2);
        let blocked = TerminalId::local(3);
        let mut workspace = Workspace::single(working.clone());
        workspace.add_window("w2".to_owned(), done.clone());
        workspace.add_window("w3".to_owned(), blocked.clone());

        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        for id in [&working, &done, &blocked] {
            panes.insert(id.clone(), PaneSlot::new_with_size(80, 24).expect("slot"));
        }
        let mut records: HashMap<TerminalId, AgentRecord> = HashMap::new();
        for (id, name, state) in [
            (&working, "w", AgentMetaState::Working),
            (&done, "d", AgentMetaState::Done),
            (&blocked, "b", AgentMetaState::Blocked),
        ] {
            records.insert(
                id.clone(),
                AgentRecord {
                    name: name.to_owned(),
                    state,
                    ..AgentRecord::default()
                },
            );
        }

        // Layout order is working, done, blocked. The ladder must reorder.
        let entries = agent_entries(&workspace, &panes, &meta_index(records.clone()));
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["b", "d", "w"],
            "blocked > unreviewed done > working"
        );

        // Visiting the finished pane demotes it below the working one: it has
        // been reviewed, so it is no longer asking for anything.
        panes.get_mut(&done).expect("slot").seen = true;
        let entries = agent_entries(&workspace, &panes, &meta_index(records));
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["b", "w", "d"],
            "a reviewed done drops below working"
        );
    }

    /// The TRIGGER half of the ladder's central promise: focusing a pane must
    /// not just flip `seen`, it must be OBSERVABLE, so the driver can recompute
    /// the chrome and repaint. The flip used to be a silent side effect at the
    /// top of the loop, one iteration AFTER the focus action already recomputed
    /// (and painted) the chrome with the stale bit — so the strip went on
    /// showing `◆ done` bold, pinned above every working agent, about the pane
    /// the user was staring at, until an unrelated chrome event fired.
    ///
    /// The contract: the flip reports `true` exactly once, that flip makes
    /// `refresh_window_chrome` report a real change (a demoted row + a new
    /// glyph), and the steady state — re-marking an already-seen pane — reports
    /// `false`, so an idle loop pass costs one hash lookup and nothing else.
    #[test]
    fn focusing_an_unreviewed_done_pane_flips_seen_and_dirties_the_chrome() {
        let working = TerminalId::local(1);
        let done = TerminalId::local(2);
        let mut workspace = Workspace::single(working.clone());
        workspace.add_window("w2".to_owned(), done.clone());

        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        for id in [&working, &done] {
            panes.insert(id.clone(), PaneSlot::new_with_size(80, 24).expect("slot"));
        }
        let mut records: HashMap<TerminalId, AgentRecord> = HashMap::new();
        for (id, name, state) in [
            (&working, "w", AgentMetaState::Working),
            (&done, "d", AgentMetaState::Done),
        ] {
            records.insert(
                id.clone(),
                AgentRecord {
                    name: name.to_owned(),
                    state,
                    ..AgentRecord::default()
                },
            );
        }
        let meta = meta_index(records);

        // The background agent finished while another pane was focused, so its
        // row is unreviewed: pinned to the top.
        let names: Vec<String> = agent_entries(&workspace, &panes, &meta)
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["d", "w"], "unreviewed done pins to the top");

        // Prime the painters against that (stale) view — this is the paint the
        // focus action itself produced, one iteration before the flip.
        let mut sidebar_painter = SidebarPainter::new(crate::render::Theme::default());
        let mut vcs = VcsIndex::default();
        refresh_window_chrome(
            None,
            &mut sidebar_painter,
            &workspace,
            &panes,
            Some(&done),
            None,
            None,
            &meta,
            &mut vcs,
        );

        // The user is now looking at the finished pane.
        assert!(
            mark_focused_seen(&mut panes, Some(&done)),
            "the first mark after a focus change must report the flip"
        );

        // The flip must move the chrome: the row demotes below the working
        // agent, and its glyph stops shouting.
        let chrome_changed = refresh_window_chrome(
            None,
            &mut sidebar_painter,
            &workspace,
            &panes,
            Some(&done),
            None,
            None,
            &meta,
            &mut vcs,
        );
        assert!(
            chrome_changed,
            "the seen flip must dirty the chrome, or nothing repaints the strip"
        );
        let entries = agent_entries(&workspace, &panes, &meta);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["w", "d"],
            "the reviewed row drops below working"
        );
        // Only the FOCUSED pane's row is reviewed — the background `working`
        // one is still unvisited, and the glyph derives from this bit.
        let reviewed: Vec<(&str, bool)> =
            entries.iter().map(|e| (e.name.as_str(), e.seen)).collect();
        assert_eq!(
            reviewed,
            vec![("w", false), ("d", true)],
            "the focused pane's row — and only it — must carry the reviewed bit"
        );

        // Steady state: no flip, no chrome change, no paint.
        assert!(
            !mark_focused_seen(&mut panes, Some(&done)),
            "re-marking an already-seen pane must not report a flip"
        );
        assert!(
            !refresh_window_chrome(
                None,
                &mut sidebar_painter,
                &workspace,
                &panes,
                Some(&done),
                None,
                None,
                &meta,
                &mut vcs,
            ),
            "an unchanged chrome must stay zero-cost"
        );
    }

    /// Equal-rank rows break the tie on the last-change clock: the agent that
    /// JUST blocked sits above one that has been blocked for an hour. Rows with
    /// no recorded change sort last, and the sort is stable, so a tie in both
    /// keys preserves window/leaf order.
    #[test]
    fn agent_entries_break_rank_ties_by_most_recent_change() {
        let old = TerminalId::local(1);
        let fresh = TerminalId::local(2);
        let never = TerminalId::local(3);
        let mut workspace = Workspace::single(old.clone());
        workspace.add_window("w2".to_owned(), fresh.clone());
        workspace.add_window("w3".to_owned(), never.clone());

        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        let mut records: HashMap<TerminalId, AgentRecord> = HashMap::new();
        for (id, name) in [(&old, "old"), (&fresh, "fresh"), (&never, "never")] {
            panes.insert(id.clone(), PaneSlot::new_with_size(80, 24).expect("slot"));
            records.insert(
                id.clone(),
                AgentRecord {
                    name: name.to_owned(),
                    state: AgentMetaState::Blocked,
                    ..AgentRecord::default()
                },
            );
        }

        let now = std::time::Instant::now();
        let mut index = meta_index(records);
        index.change_at.insert(
            old,
            now.checked_sub(std::time::Duration::from_secs(60))
                .expect("clock has an hour of headroom"),
        );
        index.change_at.insert(fresh, now);
        // `never` has no clock entry at all.

        let entries = agent_entries(&workspace, &panes, &index);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["fresh", "old", "never"]);
    }

    /// phux-foz.9: a declared `phux.agent/v1` record produces an agents-row
    /// entry with the record's name + state; the pane's OSC title (set to a
    /// conflicting agent name here) is never consulted when a record exists.
    #[test]
    fn agent_entries_prefer_the_declared_record() {
        let id = TerminalId::local(1);
        let workspace = Workspace::single(id.clone());
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        let mut slot = PaneSlot::new_with_size(80, 24).expect("slot");
        slot.terminal.vt_write(b"\x1b]2;codex resume\x07");
        panes.insert(id.clone(), slot);
        let mut records: HashMap<TerminalId, AgentRecord> = HashMap::new();
        records.insert(
            id,
            AgentRecord {
                name: "merge-queue-w5".to_owned(),
                state: AgentMetaState::Working,
                ..AgentRecord::default()
            },
        );

        let entries = agent_entries(&workspace, &panes, &meta_index(records));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].window, 0);
        assert_eq!(
            entries[0].window_name, "1",
            "stored window name, herdr's workspace column"
        );
        assert_eq!(entries[0].name, "merge-queue-w5");
        assert_eq!(entries[0].state, AgentMetaState::Working);
        assert!(!entries[0].attention);
    }

    /// phux-foz.9: no record => the OSC-title heuristic identifies plain
    /// `claude` / `codex` CLI panes; state is `idle` until the pane's
    /// ADR-0035 asked flag flips it to `blocked`. A pane matching neither
    /// source produces no row.
    #[test]
    fn agent_entries_fall_back_to_the_title_heuristic() {
        let claude = TerminalId::local(1);
        let shell = TerminalId::local(2);
        let mut workspace = Workspace::single(claude.clone());
        workspace.add_window("scratch".to_owned(), shell.clone());
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        let mut claude_slot = PaneSlot::new_with_size(80, 24).expect("slot");
        claude_slot
            .terminal
            .vt_write(b"\x1b]2;Claude Code - ~/src/phux\x07");
        panes.insert(claude.clone(), claude_slot);
        let mut shell_slot = PaneSlot::new_with_size(80, 24).expect("slot");
        shell_slot.terminal.vt_write(b"\x1b]2;~/src/phux\x07");
        panes.insert(shell, shell_slot);

        let entries = agent_entries(&workspace, &panes, &AgentMetaIndex::default());
        assert_eq!(entries.len(), 1, "the plain shell pane must not list");
        assert_eq!(entries[0].name, "claude");
        assert_eq!(entries[0].state, AgentMetaState::Idle);
        assert!(!entries[0].attention);

        // The asked flag (ADR-0035) is the one structured state signal the
        // fallback trusts: it flips the row to blocked + attention.
        panes.get_mut(&claude).expect("slot").attention = true;
        let entries = agent_entries(&workspace, &panes, &AgentMetaIndex::default());
        assert_eq!(entries[0].state, AgentMetaState::Blocked);
        assert!(entries[0].attention);
    }

    /// phux-foz.9: a record declaring (or deriving) high attention marks
    /// the entry even without the asked flag.
    #[test]
    fn agent_entries_carry_record_attention() {
        let id = TerminalId::local(1);
        let workspace = Workspace::single(id.clone());
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        panes.insert(id.clone(), PaneSlot::new_with_size(80, 24).expect("slot"));
        let mut records: HashMap<TerminalId, AgentRecord> = HashMap::new();
        records.insert(
            id,
            AgentRecord {
                name: "reviewer".to_owned(),
                // Blocked derives high attention when none is declared.
                state: AgentMetaState::Blocked,
                ..AgentRecord::default()
            },
        );

        let entries = agent_entries(&workspace, &panes, &meta_index(records));
        assert!(entries[0].attention);
    }

    #[test]
    fn window_infos_flags_zoom_only_on_the_active_window() {
        // phux-x2hm: the active window's `zoomed` reflects the zoom state;
        // a non-active window is never marked zoomed.
        let active = TerminalId::local(1);
        let mut workspace = Workspace::single(active.clone());
        workspace.add_window("2".to_owned(), TerminalId::local(2));
        workspace.select(0); // active window is index 0
        let panes: HashMap<TerminalId, PaneSlot> = HashMap::new();

        let infos = window_infos(
            &workspace,
            &panes,
            Some(&active),
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        assert!(infos[0].zoomed, "active window reflects the zoom state");
        assert!(!infos[1].zoomed, "a non-active window is never zoomed");

        // No zoom ⇒ no window is marked.
        let infos = window_infos(
            &workspace,
            &panes,
            None,
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        assert!(!infos[0].zoomed && !infos[1].zoomed);
    }

    #[test]
    fn raw_consumer_does_not_emit_frame_ack() {
        let ack = Some((TerminalId::local(7), 42u64));
        assert_eq!(
            should_emit_frame_ack(false, ack),
            None,
            "raw mode must skip the ack even when the frame carries a seq"
        );
    }

    #[test]
    fn state_sync_consumer_emits_frame_ack() {
        let ack = Some((TerminalId::local(7), 42u64));
        assert_eq!(
            should_emit_frame_ack(true, ack.clone()),
            ack,
            "state-sync mode must forward the ack the server tracks"
        );
        assert_eq!(
            should_emit_frame_ack(true, None),
            None,
            "seq=0 / no-ack frames are never acked regardless of mode"
        );
    }

    /// ADR-0060 guard: the `rec: None` arm of `run_buffered` must behave
    /// exactly as the function did before the tee existed — the bare
    /// `StdoutSink`, and the same pre-handshake failure delivered on the
    /// cooked outer terminal (phux-roz) with no wrapper in the way.
    #[tokio::test(flavor = "current_thread")]
    async fn run_buffered_without_a_recorder_passes_the_bare_sink() {
        let socket =
            std::env::temp_dir().join(format!("phux-rec-guard-{}-absent.sock", std::process::id()));
        let err = run_buffered(
            &Dial::uds(&socket),
            AttachTarget::Last,
            PredictiveConfig::disabled(),
            None,
        )
        .await
        .expect_err("there is no server at that socket");
        assert!(
            matches!(
                err,
                AttachError::Io(_) | AttachError::Connect(_) | AttachError::Unreachable(_)
            ),
            "the unrecorded path must still fail at connect, unchanged: {err:?}"
        );
    }

    #[test]
    fn attach_error_disconnected_is_distinct_from_io() {
        let a = AttachError::Disconnected;
        let b = AttachError::Io(io::Error::other("foo"));
        assert_ne!(std::mem::discriminant(&a), std::mem::discriminant(&b),);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn handshake_waits_for_hello_ok() {
        let (client_stream, server_stream) = UnixStream::pair().expect("pair");
        let mut client = Connection::from_stream(client_stream);
        let server =
            tokio::spawn(ScriptedServer::on_stream(server_stream, ScriptSpec::new()).run());

        let res = handshake(&mut client, None).await;
        assert!(
            res.is_ok(),
            "handshake should succeed when HELLO_OK arrives"
        );
        drop(client);
        let seen = server.await.expect("scripted server task");
        assert!(
            matches!(seen.first(), Some(FrameKind::Hello { .. })),
            "first client frame must be HELLO, got {:?}",
            seen.first()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handshake_advertises_probed_default_colors() {
        let colors = TerminalDefaultColors {
            foreground: TerminalColor { r: 1, g: 2, b: 3 },
            background: TerminalColor { r: 4, g: 5, b: 6 },
        };
        let (client_stream, server_stream) = UnixStream::pair().expect("pair");
        let mut client = Connection::from_stream(client_stream);
        let server =
            tokio::spawn(ScriptedServer::on_stream(server_stream, ScriptSpec::new()).run());

        let res = handshake(&mut client, Some(colors)).await;
        assert!(res.is_ok());
        drop(client);
        let seen = server.await.expect("scripted server task");
        let Some(FrameKind::Hello { client_caps, .. }) = seen.first() else {
            panic!("first client frame must be HELLO, got {:?}", seen.first());
        };
        assert_eq!(client_caps.default_colors, Some(colors));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handshake_rejects_non_hello_ok_reply() {
        let (client_stream, server_stream) = UnixStream::pair().expect("pair");
        let mut client = Connection::from_stream(client_stream);
        let mut server = Connection::from_stream(server_stream);

        let server_side = async move {
            let frame = server.recv().await.expect("server recv hello");
            assert!(
                matches!(frame, FrameKind::Hello { .. }),
                "first client frame must be HELLO"
            );
            server
                .send(&FrameKind::Detached)
                .await
                .expect("server send detached");
        };

        let (res, ()) = tokio::join!(handshake(&mut client, None), server_side);
        match res {
            Err(AttachError::Protocol(msg)) => {
                assert!(msg.contains("HELLO_OK"));
            }
            other => panic!("expected protocol error, got {other:?}"),
        }
    }

    /// The factored builder produces a `ViewportResize` frame carrying
    /// the supplied viewport unchanged. Lets us assert the encoder-
    /// feeding side of the SIGWINCH path without firing a real signal
    /// or driving a tokio runtime.
    #[test]
    fn viewport_resize_frame_carries_viewport_unchanged() {
        let vp = ViewportInfo::new(132, 50).with_pixels(Some(1320), Some(750));
        match viewport_resize_frame(vp) {
            FrameKind::ViewportResize { viewport } => {
                assert_eq!(viewport.cols, 132);
                assert_eq!(viewport.rows, 50);
                assert_eq!(viewport.pixel_w, Some(1320));
                assert_eq!(viewport.pixel_h, Some(750));
            }
            other => panic!("expected ViewportResize, got {other:?}"),
        }
    }

    /// `current_viewport_or_default` returns _something_ even when stdout
    /// isn't a TTY (cargo test path). The exact dims aren't load-bearing
    /// — what matters is that we never return an error and always have a
    /// frame to send.
    #[test]
    fn current_viewport_or_default_never_panics() {
        let vp = current_viewport_or_default();
        // Cell dims fit in u16 by construction; just exercise the path.
        let _ = (vp.cols, vp.rows);
    }

    /// Borrow a real `Termios` from `/dev/tty` so tests that need to
    /// exercise [`save_termios_snapshot`] / [`take_termios_snapshot`]
    /// can run with a plausible value. Returns `None` when the test
    /// process has no controlling TTY (e.g. some CI sandboxes); the
    /// caller skips in that case.
    fn try_borrow_real_termios() -> Option<Termios> {
        let tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .ok()?;
        rustix::termios::tcgetattr(tty.as_fd()).ok()
    }

    /// The save/take helpers behind [`SAVED_TERMIOS`] round-trip a
    /// snapshot exactly once: after a save, the next take returns
    /// `Some(_)`; subsequent takes return `None`. This is the unit
    /// surface that backs the signal-arm true-restore path
    /// (phux-2r7). The signal arm itself is exercised by a manual
    /// SIGINT during an attach session — see the comment on
    /// [`terminal_reset_on_signal`].
    ///
    /// `SAVED_TERMIOS` is a process-global; we clear at both ends to
    /// be hygienic across the in-test serial execution model.
    #[test]
    fn saved_termios_round_trip() {
        let Some(t) = try_borrow_real_termios() else {
            // No controlling TTY in this test process; nothing to
            // assert. The save/take helpers are still type-checked.
            return;
        };
        // Pre-clean: another test (or a panic) may have left state.
        let _ = take_termios_snapshot();
        assert!(take_termios_snapshot().is_none());

        save_termios_snapshot(t);
        assert!(
            take_termios_snapshot().is_some(),
            "save then take must return the snapshot"
        );
        assert!(
            take_termios_snapshot().is_none(),
            "second take must be empty"
        );
    }

    /// Documents the manual SIGINT repro that backs phux-2r7. The
    /// signal-arm path can't be unit-tested without forking and
    /// driving a real PTY; this `#[ignore]`-stub keeps the procedure
    /// next to the code and surfaces in `cargo test -- --ignored` if
    /// someone wires up an integration harness later.
    /// ADR-0048: with mouse capture on, the alt-screen entry sequence also
    /// enables the client's own outer-terminal mouse tracking
    /// (`?1002h` button-motion + `?1006h` SGR), and the reset undoes it
    /// (`?1006l?1002l`) BEFORE leaving the alt screen so the host's native
    /// selection is restored.
    #[test]
    fn mouse_capture_enable_and_disable_bytes() {
        let _guard = TERMINAL_RESET_TEST_LOCK
            .lock()
            .expect("terminal reset test lock");
        MOUSE_CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
        ALT_SCREEN_ACTIVE.store(false, Ordering::SeqCst);

        let mut entry = Vec::new();
        write_enter_alt_screen(&mut entry, true).unwrap();
        assert!(
            entry.windows(8).any(|w| w == b"\x1b[?1002h"),
            "entry must enable button-motion tracking: {entry:?}"
        );
        assert!(
            entry.windows(8).any(|w| w == b"\x1b[?1006h"),
            "entry must enable SGR coordinates: {entry:?}"
        );
        // `write_enter_alt_screen` records MOUSE_CAPTURE_ACTIVE; the
        // alt-screen flag is set separately by `install_with_stdout` on a
        // real attach. Set it here so reset exercises the full leave path
        // (mouse-disable AND alt-screen-leave) the way a live detach does.
        ALT_SCREEN_ACTIVE.store(true, Ordering::SeqCst);
        // Reset emits the leave pair before the ?1049l alt-screen leave.
        let mut reset = Vec::new();
        write_terminal_reset(&mut reset).unwrap();
        let pos_1006l = reset
            .windows(8)
            .position(|w| w == b"\x1b[?1006l")
            .expect("reset must disable SGR coordinates");
        let pos_1002l = reset
            .windows(8)
            .position(|w| w == b"\x1b[?1002l")
            .expect("reset must disable button-motion");
        let pos_1049l = reset
            .windows(8)
            .position(|w| w == b"\x1b[?1049l")
            .expect("reset must leave the alt screen");
        assert!(
            pos_1006l < pos_1049l && pos_1002l < pos_1049l,
            "mouse-disable must precede the alt-screen leave: {reset:?}"
        );
    }

    /// ADR-0048: `mouse = false` skips the DECSET entirely — the entry
    /// sequence emits no mouse tracking, host native selection untouched.
    #[test]
    fn mouse_capture_disabled_emits_no_decset() {
        let _guard = TERMINAL_RESET_TEST_LOCK
            .lock()
            .expect("terminal reset test lock");
        MOUSE_CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
        ALT_SCREEN_ACTIVE.store(false, Ordering::SeqCst);

        let mut entry = Vec::new();
        write_enter_alt_screen(&mut entry, false).unwrap();
        assert!(
            !entry.windows(8).any(|w| w == b"\x1b[?1002h"),
            "mouse=false must not enable tracking: {entry:?}"
        );
        assert!(
            entry.windows(8).any(|w| w == b"\x1b[?1049h"),
            "alt-screen enter still emitted: {entry:?}"
        );
        // With capture never set, reset emits no mouse-disable bytes.
        let mut reset = Vec::new();
        write_terminal_reset(&mut reset).unwrap();
        assert!(
            !reset.windows(8).any(|w| w == b"\x1b[?1002l"),
            "no capture ⇒ no mouse-disable on reset: {reset:?}"
        );
        assert!(
            !reset.windows(8).any(|w| w == b"\x1b[?1006l"),
            "no capture ⇒ no SGR mouse-disable on reset: {reset:?}"
        );
    }

    /// phux-npb3: `sync_mouse_capture` reconciles the outer DECSET with the
    /// desired state — leave pair when dropping, enter pair when restoring,
    /// and nothing at all when the state already matches.
    #[test]
    fn sync_mouse_capture_emits_transitions_only() {
        let _guard = TERMINAL_RESET_TEST_LOCK
            .lock()
            .expect("terminal reset test lock");
        MOUSE_CAPTURE_ACTIVE.store(true, Ordering::SeqCst);

        // Already on ⇒ no bytes.
        let mut out = Vec::new();
        sync_mouse_capture(&mut out, true).unwrap();
        assert!(out.is_empty(), "no transition ⇒ no bytes: {out:?}");

        // On → off emits the reverse-order leave pair.
        sync_mouse_capture(&mut out, false).unwrap();
        assert_eq!(out, b"\x1b[?1006l\x1b[?1002l");

        // Off is now recorded ⇒ a second off is a no-op.
        out.clear();
        sync_mouse_capture(&mut out, false).unwrap();
        assert!(out.is_empty(), "idempotent off ⇒ no bytes: {out:?}");

        // Off → on emits the entry pair, and the reset path sees capture as
        // active again (the shared MOUSE_CAPTURE_ACTIVE flag).
        sync_mouse_capture(&mut out, true).unwrap();
        assert_eq!(out, b"\x1b[?1002h\x1b[?1006h");
        assert!(MOUSE_CAPTURE_ACTIVE.load(Ordering::SeqCst));

        MOUSE_CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
    }

    /// phux-wrnm: hover reporting is raised only while something consumes
    /// it, only on top of live capture, and always unwound — including when
    /// capture itself drops while a menu is still open.
    #[test]
    fn sync_hover_tracking_rides_on_top_of_capture() {
        let _guard = TERMINAL_RESET_TEST_LOCK
            .lock()
            .expect("terminal reset test lock");
        MOUSE_CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
        HOVER_TRACKING_ACTIVE.store(false, Ordering::SeqCst);

        // No capture ⇒ the client has no business reporting motion.
        let mut out = Vec::new();
        sync_hover_tracking(&mut out, true).unwrap();
        assert!(out.is_empty(), "capture off ⇒ no hover bytes: {out:?}");
        assert!(!HOVER_TRACKING_ACTIVE.load(Ordering::SeqCst));

        // With capture live, opening a menu raises any-motion once.
        MOUSE_CAPTURE_ACTIVE.store(true, Ordering::SeqCst);
        sync_hover_tracking(&mut out, true).unwrap();
        assert_eq!(out, b"\x1b[?1003h");
        out.clear();
        sync_hover_tracking(&mut out, true).unwrap();
        assert!(out.is_empty(), "no transition ⇒ no bytes: {out:?}");

        // Closing it drops any-motion and re-asserts button-event tracking,
        // so divider drags survive the round trip.
        sync_hover_tracking(&mut out, false).unwrap();
        assert_eq!(out, b"\x1b[?1003l\x1b[?1002h");

        // Capture dropping (focus moved to an opted-out pane) while a menu
        // is open must not strand `?1003h` on the host terminal.
        out.clear();
        sync_hover_tracking(&mut out, true).unwrap();
        assert_eq!(out, b"\x1b[?1003h");
        out.clear();
        sync_mouse_capture(&mut out, false).unwrap();
        assert_eq!(out, b"\x1b[?1003l\x1b[?1006l\x1b[?1002l");
        assert!(!HOVER_TRACKING_ACTIVE.load(Ordering::SeqCst));

        MOUSE_CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
    }

    /// phux-npb3: capture follows focus — wanted iff the global gate is on
    /// AND the focused pane has not opted out.
    #[test]
    fn desired_mouse_capture_follows_focused_pane_optout() {
        let t1 = TerminalId::local(1);
        let t2 = TerminalId::local(2);
        let mut optout = std::collections::HashSet::new();
        optout.insert(t2.clone());

        // Global gate off wins unconditionally.
        assert!(!desired_mouse_capture(false, Some(&t1), &optout));
        assert!(!desired_mouse_capture(false, None, &optout));
        // Gate on: an opted-in focused pane (or none yet) keeps capture.
        assert!(desired_mouse_capture(true, Some(&t1), &optout));
        assert!(desired_mouse_capture(true, None, &optout));
        // Gate on but the focused pane opted out ⇒ capture drops.
        assert!(!desired_mouse_capture(true, Some(&t2), &optout));
    }

    #[test]
    #[ignore = "manual: requires a live PTY and a SIGINT during attach"]
    fn signal_arm_true_restore_manual_repro() {
        // 1. `stty -a` in an outer shell; note `iutf8` / VEOF / etc.
        // 2. `phux attach <session>` — driver enters raw mode + alt
        //    screen; `RawModeGuard::install_with_stdout` parks the
        //    pre-attach Termios in `SAVED_TERMIOS`.
        // 3. In a sibling shell: `kill -INT <phux-pid>` (or hit Ctrl-C
        //    if your outer shell forwards it without phux eating it).
        // 4. `stty -a` again; ALL flags should match step (1). Before
        //    phux-2r7, only ICANON|ECHO|ISIG round-tripped and custom
        //    flags like `iutf8` were lost.
    }

    // -----------------------------------------------------------------
    // phux-foz.10: chrome persists while overlays are open.
    // -----------------------------------------------------------------

    use crate::render::overlay::{RenderOverlay, SelectItem, SelectList};
    use phux_config::KeybindingsCfg;
    use phux_config::keybind::ResolvedAction;
    use phux_config::widget::WindowInfo;

    /// The probe viewport for the overlay-chrome tests.
    const PROBE_VIEW: (u16, u16) = (80, 24);
    /// Sidebar strip width for the overlay-chrome tests.
    const PROBE_SIDEBAR_W: u16 = 20;
    /// Window label shown on the sidebar's name row. Distinctive: appears
    /// nowhere in any pane content or overlay body, so finding it in the
    /// replayed frame proves the strip painted.
    const PROBE_WINDOW: &str = "w1-agent";
    /// Branch shown on the sidebar's branch row (herdr-style, phux-p4vp).
    const PROBE_BRANCH: &str = "foz10-br";
    /// Content written into the pane mirror, to prove the base frame
    /// repainted around the floating modal.
    const PROBE_PANE_TEXT: &str = "PANE-BASE";

    /// Replay `bytes` (a full frame of VT output) into a fresh libghostty
    /// terminal — the house PTY-probe oracle — and project the resulting
    /// grid to row-major plain text via the same `render_at_cells` surface
    /// the production compositor uses.
    fn replay_rows(bytes: &[u8]) -> Vec<String> {
        let (cols, rows) = PROBE_VIEW;
        let mut probe = PaneSlot::new_with_size(cols, rows).expect("probe slot");
        probe.terminal.vt_write(bytes);
        let mut frame = phux_core::screen::RenderedFrame::blank(cols, rows);
        probe
            .renderer
            .render_at_cells(&probe.terminal, &mut frame, (0, 0), (cols, rows))
            .expect("project probe cells");
        (0..rows)
            .map(|r| {
                let base = usize::from(r) * usize::from(cols);
                frame.cells[base..base + usize::from(cols)]
                    .iter()
                    .map(|c| c.grapheme.as_str())
                    .collect::<String>()
            })
            .collect()
    }

    /// The sidebar strip columns (left dock) of every replayed row, joined
    /// as one string per row.
    fn strip_columns(rows: &[String]) -> Vec<String> {
        rows.iter()
            .map(|r| r.chars().take(usize::from(PROBE_SIDEBAR_W)).collect())
            .collect()
    }

    /// One `paint_active_overlay` frame for `overlay`, with the sidebar
    /// enabled (left, width 20) and its painter threaded when
    /// `with_painter`. Returns the emitted VT bytes.
    fn paint_overlay_frame(overlay: Box<dyn RenderOverlay>, with_painter: bool) -> Vec<u8> {
        let theme = crate::render::Theme::default();
        let id = TerminalId::local(1);
        let workspace = Workspace::single(id.clone());
        let sidebar = Some(SidebarReservation {
            edge: SidebarEdge::Left,
            width: PROBE_SIDEBAR_W,
        });
        // Pane mirror sized to the content rect (80 - 20 sidebar cols, no
        // status bar) so the letterboxed paint fills its rect exactly.
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        let mut slot = PaneSlot::new_with_size(PROBE_VIEW.0 - PROBE_SIDEBAR_W, PROBE_VIEW.1)
            .expect("pane slot");
        slot.terminal.vt_write(PROBE_PANE_TEXT.as_bytes());
        panes.insert(id.clone(), slot);

        let mut sidebar_painter = SidebarPainter::new(theme);
        sidebar_painter.set_windows(vec![WindowInfo {
            name: PROBE_WINDOW.to_owned(),
            active: true,
            zoomed: false,
            attention: false,
            branch: Some(PROBE_BRANCH.to_owned()),
        }]);

        let mut overlays = OverlayState::new();
        overlays.push(overlay);

        let mut out: Vec<u8> = Vec::new();
        paint_active_overlay(
            &mut out,
            &overlays,
            &workspace,
            &mut panes,
            Some(&id),
            None,
            PROBE_VIEW,
            None,
            sidebar,
            with_painter.then_some(&mut sidebar_painter),
            "probe",
            &theme,
        );
        out
    }

    /// The command palette, as the dispatcher builds it (`SelectList`).
    fn palette_overlay() -> Box<dyn RenderOverlay> {
        let theme = crate::render::Theme::default();
        let items = vec![
            SelectItem::new(
                "detach",
                ResolvedAction {
                    action: "detach".to_owned(),
                    args: std::collections::BTreeMap::new(),
                },
            ),
            SelectItem::new(
                "new-window",
                ResolvedAction {
                    action: "new-window".to_owned(),
                    args: std::collections::BTreeMap::new(),
                },
            ),
        ];
        Box::new(SelectList::new("command palette", items, &theme))
    }

    /// The agent-fleet dashboard, as the dispatcher builds it (phux-foz.7):
    /// a `SelectList` carrying the fleet live key, with rows from
    /// [`crate::attach::fleet::fleet_items`]. It rides the same bounded
    /// floating-modal path as the palette, and the driver's fleet-dirty
    /// live-refresh repaints it through `paint_active_overlay` — so it must
    /// keep the sidebar visible on every refresh frame too.
    fn fleet_overlay() -> Box<dyn RenderOverlay> {
        let theme = crate::render::Theme::default();
        let workspace = Workspace::single(TerminalId::local(1));
        let items = crate::attach::fleet::fleet_items(
            &workspace,
            &[],
            None,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert!(
            !items.iter().all(SelectItem::is_header),
            "probe fleet dashboard must have selectable rows"
        );
        Box::new(
            SelectList::new("agent fleet", items, &theme)
                .with_live_key(crate::attach::fleet::FLEET_LIVE_KEY),
        )
    }

    /// phux-foz.10 mechanism guard: this pins the DEFECT shape so the
    /// regression tests below cannot false-pass. A floating-modal repaint
    /// whose base frame omits the sidebar painter leaves the reserved strip
    /// columns blank — the "sidebar vanishes while the palette is open" bug.
    #[test]
    fn overlay_base_frame_without_painter_blanks_the_sidebar() {
        let rows = replay_rows(&paint_overlay_frame(palette_overlay(), false));
        let strip = strip_columns(&rows).join("\n");
        assert!(
            !strip.contains(PROBE_WINDOW) && !strip.contains(PROBE_BRANCH),
            "probe must detect the blank strip when the painter is absent;\n{strip}"
        );
    }

    /// phux-foz.10: opening the command palette must NOT blank the sidebar.
    /// The floating-modal base frame repaints the strip (window label +
    /// branch line) and the panes, then paints the modal on top.
    #[test]
    fn command_palette_keeps_sidebar_visible() {
        let rows = replay_rows(&paint_overlay_frame(palette_overlay(), true));
        let all = rows.join("\n");
        let strip = strip_columns(&rows).join("\n");
        assert!(
            strip.contains(PROBE_WINDOW),
            "sidebar window label must survive the palette;\n{all}"
        );
        assert!(
            strip.contains(PROBE_BRANCH),
            "sidebar branch line must survive the palette;\n{all}"
        );
        assert!(
            all.contains("command palette"),
            "the palette itself must be painted on top;\n{all}"
        );
        assert!(
            all.contains(PROBE_PANE_TEXT),
            "pane content must stay visible around the floating modal;\n{all}"
        );
        // phux-foz.14: the modal centers inside the pane content rect, so its
        // box corners land right of the sidebar divider — never inside the
        // reserved strip columns (the sidebar draws no corner glyphs itself).
        assert!(
            !strip.contains('┌') && !strip.contains('└'),
            "modal box corners must not intrude into the sidebar columns;\n{strip}"
        );
        // Pin the exact composition: sidebar strip + pane + centered modal.
        insta::assert_snapshot!(
            "palette_over_sidebar",
            rows.iter()
                .map(|r| r.trim_end())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    /// phux-foz.10: every bounded (floating) overlay kind shares the same
    /// base-frame path, so which-key, help, prompts, pickers, and toasts
    /// must all keep the sidebar visible too.
    #[test]
    fn all_floating_overlays_keep_sidebar_visible() {
        let theme = crate::render::Theme::default();
        let wk_cfg = KeybindingsCfg {
            prefix_table: std::iter::once((
                "d".to_owned(),
                phux_config::Action::Bare("detach".to_owned()),
            ))
            .collect(),
            ..KeybindingsCfg::default()
        };
        let overlays: Vec<(&str, Box<dyn RenderOverlay>)> = vec![
            ("palette", palette_overlay()),
            // phux-foz.7 fleet dashboard: same floating-modal path, and the
            // driver's fleet-dirty live refresh repaints it while it is
            // open — the sidebar must survive every refresh frame.
            ("agent-fleet", fleet_overlay()),
            (
                "which-key",
                Box::new(crate::render::overlay::WhichKeyOverlay::from_config(
                    &wk_cfg, &theme,
                )),
            ),
            (
                "help",
                Box::new(crate::render::overlay::HelpOverlay::from_config(
                    &wk_cfg, &theme,
                )),
            ),
            (
                "prompt",
                Box::new(crate::render::overlay::PromptOverlay::new(
                    "rename window",
                    "rename-window",
                    "name",
                    "1",
                    &theme,
                )),
            ),
            (
                "toast",
                Box::new(crate::render::overlay::ToastOverlay::new(
                    "notice",
                    vec!["a line".to_owned()],
                    &theme,
                )),
            ),
        ];
        for (label, overlay) in overlays {
            let rows = replay_rows(&paint_overlay_frame(overlay, true));
            let strip = strip_columns(&rows).join("\n");
            assert!(
                strip.contains(PROBE_WINDOW),
                "{label}: sidebar window label must survive the overlay;\n{}",
                rows.join("\n")
            );
            assert!(
                strip.contains(PROBE_BRANCH),
                "{label}: sidebar branch line must survive the overlay;\n{}",
                rows.join("\n")
            );
        }
    }

    /// The copy-mode status strip counts a block selection as
    /// `span_rows * band_cols`, distinct from the linear bounding-box count,
    /// and never underflows when the tuple-normalized corners leave
    /// `start_col > end_col` (a multi-row up-left drag).
    #[test]
    fn copy_mode_status_block_cell_count_differs_from_linear() {
        let theme = crate::render::Theme::default();
        let status_of = |sel: SelectionRect| -> String {
            let mut out: Vec<u8> = Vec::new();
            paint_copy_mode_status(&mut out, sel, (80, 24), &theme).expect("status");
            String::from_utf8_lossy(&out).into_owned()
        };

        // Corners tuple-normalize to start=(0,5), end=(2,2): 3 spanned rows,
        // column band {2,3,4,5} = 4 wide. Note start_col (5) > end_col (2).
        let corners = |rectangle| SelectionRect {
            start_row: 0,
            start_col: 5,
            end_row: 2,
            end_col: 2,
            rectangle,
        };

        // Block: 3 rows * 4 band cols = 12 (and no underflow despite 5 > 2).
        assert!(
            status_of(corners(true)).contains("12 cell(s)"),
            "block count must be span_rows * band_cols = 12"
        );
        // Linear: the bounding-box arithmetic saturates the reversed columns to
        // a width of 1, giving 3 rows * 1 = 3 — a different number, proving the
        // branch is taken and that the shared corners no longer panic.
        assert!(
            status_of(corners(false)).contains("3 cell(s)"),
            "linear count must differ from the block count"
        );

        // A plainly-ordered block (start_col <= end_col) counts the full band.
        let ordered_block = SelectionRect {
            start_row: 1,
            start_col: 2,
            end_row: 3,
            end_col: 6,
            rectangle: true,
        };
        // 3 rows * band {2..=6} (5 wide) = 15.
        assert!(
            status_of(ordered_block).contains("15 cell(s)"),
            "ordered block: 3 rows * 5 band cols = 15"
        );
    }
}
