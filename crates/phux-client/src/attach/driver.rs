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
//! * a local `libghostty_vt::Terminal` + [`super::render::TerminalRenderer`] for the focused
//!   pane (under ADR-0013 the client is bytes-in / `vt_write` / dirty-row
//!   redraw — see `research/2026-05-25-libghostty-renderstate.md`).

#![allow(
    clippy::result_large_err,
    reason = "AttachError carries an io::Error which is naturally large; the variants are mutually exclusive and we never carry the result in a hot loop."
)]

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::{self, IsTerminal, Write};
use std::os::fd::AsFd;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use phux_client_core::engine::ghostty::GhosttyAdapter;
use phux_client_core::history::HistoryCacheConfig;
use phux_client_core::session::{EffectBuffer as KernelEffectBuffer, SessionKernel};
#[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::caps::{
    BootstrapLimits, ClientCapabilities, Layer, LayerSet, OutputMode, ServerFeature,
    detect_color_support,
};
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
    sync_overlays_to_focused_pane,
};
use super::outcome::{AttachEnd, AttachError};
use super::paint::{
    SidebarEdge, SidebarReservation, StatusBarPaint, content_rect, paint_bar_after_pane,
    paint_chrome_in_place, paint_full_frame, sidebar_reservation,
};
use super::pane_state::{
    AttachKernel, AttentionNavigation, PaneSlot, VcsIndex, reanchor_predict_to_pane,
};
#[cfg(test)]
use super::pane_state::{clear_attention_on_input, published_test_state};
use super::plugin_actions::{self, PluginActionEntry, PluginRunResult};
use super::plugin_panes;
use super::record::{SessionRecorder, TeeSink};
use super::render::{SelectionRect, write_cup, write_reset};
use super::repaint::{RepaintAccumulator, RepaintLevel};
use super::server_frame::{AgentMetaIndex, FrameOutcome, handle_server_frame};
use crate::agent_meta::{
    AgentAttention, AgentMetaState, AgentRecord, TERMINAL_AGENT_KEY, agent_name_from_title,
    parse_agent_record,
};
use crate::layout::Workspace;
#[cfg(test)]
use crate::layout_ops::LAYOUT_KEY;
use crate::layout_ops::{DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, layout_key};
use crate::predict::{Overlay, PredictionState, PredictiveConfig};
use crate::render::ChromeBreakpoints;
use crate::render::chrome::sidebar::{AgentEntry, SidebarPainter, attention_rank};
use crate::render::chrome::status_bar::{Notice, StatusBarPainter};
use crate::render::overlay::OverlayState;
use phux_config::SidebarPosition;

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
    changed |= sidebar_painter.set_needs_you(agent_entries(workspace, panes, agent_meta));
    changed
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
        FrameKind::TerminalOutput { terminal_id, .. } => Some(terminal_id),
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

/// Apply the per-pane last-wins coalescing decision.
const fn frame_defers_paint(deferred_by_coalesce: bool, _frame: &FrameKind) -> bool {
    deferred_by_coalesce
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
    engine_kernel: &AttachKernel,
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
) -> StatusBarPaint {
    // phux-foz.14: floating modals center inside the pane content rect (the
    // viewport minus the sidebar strip and status-bar row), NOT the raw
    // viewport, so a centered box never lands on the sidebar columns and
    // occludes the chrome the base-frame repaint (phux-foz.10) preserves. The
    // borrow of `status_bar` ends here (position is `Copy`), so it stays
    // available to move into `paint_full_frame` below.
    let bar_pos = status_bar.as_deref().map(StatusBarPainter::position);
    let overlay_content = {
        let cr = content_rect(viewport_dims, bar_pos, sidebar);
        ratatui::layout::Rect::new(cr.x, cr.y, cr.w, cr.h)
    };
    if let Some(sel) = overlays.copy_selection() {
        let (Some(ls), Some(fid)) = (workspace.active_window(), focused) else {
            return StatusBarPaint::NotPublished;
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
        let painted = base
            .as_deref()
            .map_or(StatusBarPaint::NotPublished, |base| {
                paint_full_frame(
                    out,
                    base,
                    panes,
                    engine_kernel,
                    focused,
                    viewport_dims,
                    status_bar,
                    sidebar,
                    sidebar_painter,
                    session_name,
                )
            });
        if let Some(slot) = panes.get_mut(fid) {
            slot.renderer.set_selection(None);
        }
        let _ = paint_copy_mode_status(out, sel, viewport_dims, theme);
        if matches!(
            bar_pos,
            Some(crate::render::chrome::status_bar::Position::Bottom)
        ) {
            StatusBarPaint::NotPublished
        } else {
            painted
        }
    } else if let Some(clip) = overlays.active_bounds(overlay_content) {
        // Floating modal (help / prompt / command palette / pickers): keep
        // the live panes visible by repainting the base frame, then emit
        // only the modal's bounded region on top. No `\x1b[2J` — the panes
        // surround the box instead of vanishing behind a full-screen clear.
        // The base frame includes the sidebar strip (phux-foz.10): the
        // repaint's own ED2 cleared it, and chrome must persist under a
        // floating overlay.
        let painted =
            workspace
                .render_window(zoomed)
                .as_deref()
                .map_or(StatusBarPaint::NotPublished, |ls| {
                    paint_full_frame(
                        out,
                        ls,
                        panes,
                        engine_kernel,
                        focused,
                        viewport_dims,
                        status_bar,
                        sidebar,
                        sidebar_painter,
                        session_name,
                    )
                });
        let _ = overlays.paint_clipped(out, viewport_dims, overlay_content, clip, theme.shadow);
        painted
    } else {
        // Full-screen overlay (no bounded region): clear + paint.
        let _ = out.write_all(b"\x1b[2J\x1b[H");
        let _ = overlays.paint(out, viewport_dims);
        StatusBarPaint::NotPublished
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
    initial_notice: Option<Notice>,
) -> Result<AttachEnd, AttachError> {
    let (mut sink, writer) = super::stdout_writer::spawn_stdout_writer();
    // Cloned BEFORE any wrap: the resync flag belongs to the StdoutSink, not
    // to whatever is layered on top of it.
    let resync = Arc::clone(&sink.needs_resync);
    if let Some(rec) = rec {
        let mut tee = TeeSink {
            inner: &mut sink,
            rec: Rc::clone(&rec),
        };
        attach_session(
            dial,
            target,
            &mut tee,
            predict,
            Some(resync.as_ref()),
            Some(writer),
            true,
            initial_notice,
            Some(rec),
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
            initial_notice,
            None,
        )
        .await
    }
}

/// Dial-aware production attach (UDS *or* QUIC) with predictive echo config.
///
/// The CLI builds a [`Dial`] from its flags (a UDS path or a remote
/// `--quic` target) and the same off-loop-stdout production path runs
/// regardless of byte plumbing. Blocks until the server sends `DETACHED`
/// or the user detaches.
///
/// The function is `async` because it relies on tokio; embedders must
/// drive it on a tokio runtime. Per ADR-0003 the canonical runtime is
/// `tokio::runtime::Builder::new_current_thread` — the returned future
/// is intentionally `!Send` because libghostty's `Terminal` is `!Send`
/// and lives on the attach task's stack across `await` points. The
/// single-threaded runtime never moves the future between threads.
///
/// `predict.enabled = false` bypasses prediction entirely;
/// `predict.enabled = true` engages the Mosh-class prediction layer
/// documented in [`crate::predict`] (`phux-9gw.1`).
///
/// # Ordering (`phux-roz`)
///
/// The expensive pre-handshake work — connect, `HELLO`, `ATTACH`,
/// and the `ATTACHED` wait — runs on the *cooked* outer terminal.
/// Failures there propagate as `Err(_)` without ever entering raw mode
/// or the alt screen, so a missing server / bad session name / Ctrl-C
/// during connect prints a one-line error on the normal screen and
/// exits cleanly. Only after the server's `ATTACHED` frame arrives do
/// we flip the terminal into raw + alt screen via [`RawModeGuard`].
///
/// `initial_notice` (phux-i0e8.2.3) is a transient status-bar message shown
/// once the session is attached and painting — the CLI's reconnect loop
/// passes `re-attached after server restart` so the recovery is visible
/// *inside* the TUI (a cooked-terminal eprintln is alt-screened over within
/// milliseconds). `None` on a first attach.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub async fn run_with_predict_dial(
    dial: &Dial,
    target: AttachTarget,
    predict: PredictiveConfig,
    initial_notice: Option<Notice>,
) -> Result<AttachEnd, AttachError> {
    run_buffered(dial, target, predict, None, initial_notice).await
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
    initial_notice: Option<Notice>,
) -> Result<AttachEnd, AttachError> {
    run_buffered(dial, target, predict, Some(rec), initial_notice).await
}

/// UDS attach that writes the entire composited output stream to a
/// caller-supplied [`RenderSink`](super::RenderSink) (any `Write`).
///
/// The stream covers alt-screen enter, cursor hide, every pane's per-row
/// CUP/SGR, the status bar, overlays, and cleanup.
///
/// The renderer and all chrome painters are generic over `Write`, and the
/// driver threads this one sink through `main_loop` into
/// `handle_server_frame`, `paint_full_frame`, and `dispatch_input_events`.
/// So the whole attach render path is injectable: production passes real
/// stdout via [`run_with_predict_dial`]; tests and the headless agent
/// surface pass a `Vec<u8>` (or any other `Write`) and read back the
/// captured VT.
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
) -> Result<AttachEnd, AttachError> {
    run_with_stdout_predict(socket, target, out, PredictiveConfig::disabled()).await
}

/// As [`run_with_stdout`], but with an explicit predictive-echo config.
/// Production callers should reach for [`run_with_predict_dial`]; this is
/// the test-injectable variant.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub async fn run_with_stdout_predict<W: super::RenderSink>(
    socket: &Path,
    target: AttachTarget,
    out: &mut W,
    predict: PredictiveConfig,
) -> Result<AttachEnd, AttachError> {
    // Synchronous-sink test seam: no off-loop writer, no resync flag.
    attach_session(
        &Dial::uds(socket),
        target,
        out,
        predict,
        None,
        None,
        false,
        None,
        None,
    )
    .await
}
type HeadlessHistoryGeneration = (
    TerminalId,
    phux_protocol::StreamId,
    phux_protocol::BootstrapId,
);

#[derive(Debug, Default)]
struct HeadlessCompletion {
    attach_ready: bool,
    pending_history: HashSet<HeadlessHistoryGeneration>,
    pending_layout: Option<u32>,
}

impl HeadlessCompletion {
    fn new(pending_layout: Option<u32>) -> Self {
        Self {
            pending_layout,
            ..Self::default()
        }
    }

    fn observe_frame(&mut self, frame: &FrameKind, attach_id: u32) {
        match frame {
            FrameKind::AttachReady {
                attach_id: ready_id,
            } if *ready_id == attach_id => self.attach_ready = true,
            FrameKind::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                next_cursor: None,
                ..
            }
            | FrameKind::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            }
            | FrameKind::HistoryRejected {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } => {
                self.pending_history
                    .remove(&(terminal_id.clone(), *stream_id, *bootstrap_id));
            }
            FrameKind::MetadataValue { request_id, .. }
                if self.pending_layout == Some(*request_id) =>
            {
                self.pending_layout = None;
            }
            _ => {}
        }
    }

    fn note_history_request(
        &mut self,
        terminal_id: &TerminalId,
        stream_id: phux_protocol::StreamId,
        bootstrap_id: phux_protocol::BootstrapId,
    ) {
        self.pending_history
            .insert((terminal_id.clone(), stream_id, bootstrap_id));
    }
    fn restart_attach(&mut self) {
        self.attach_ready = false;
        self.pending_history.clear();
    }

    fn is_complete(&self, agent_metadata_complete: bool) -> bool {
        self.attach_ready
            && self.pending_history.is_empty()
            && self.pending_layout.is_none()
            && agent_metadata_complete
    }
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
/// Completion policy (R3): after the ATTACHED replay and one-shot metadata
/// requests, frames are drained until the matching `ATTACH_READY`, every
/// requested history cursor chain, and every required metadata reply complete.
/// The overall deadline is an error, never partial or blank success.
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

    /// Hard cap on waiting for the matching aggregate attach barrier.
    const ATTACH_READY_DEADLINE: Duration = Duration::from_secs(3);

    let client_caps = attach_client_caps(None);
    let mut conn =
        Connection::connect_with_hello(socket, attach_client_name(), client_caps).await?;
    let negotiated = conn.negotiated_bootstrap().ok_or_else(|| {
        AttachError::Protocol("headless attach lacks negotiated bootstrap".to_owned())
    })?;
    let terminal_reply_supported = negotiated
        .server_features
        .contains(ServerFeature::TerminalReply);
    let history_config = HistoryCacheConfig {
        request_max_bytes: negotiated.limits.max_history_page_bytes(),
        ..HistoryCacheConfig::default()
    };
    let mut engine_kernel = SessionKernel::with_history_config(
        GhosttyAdapter::new(negotiated.limits),
        negotiated.profile,
        history_config,
    );
    let mut kernel_effects = KernelEffectBuffer::new();
    let mut attach_id = send_attach(&mut conn, target).await?;
    let attached = wait_for_attached(&mut conn, attach_id).await?;

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
    // phux-huhi: the same `[chrome]` thresholds a live attach folds in, so a
    // rendered snapshot yields the sidebar at the width the user configured.
    let headless_chrome = headless_cfg
        .as_ref()
        .map_or_else(ChromeBreakpoints::default, |c| {
            ChromeBreakpoints::from_cfg(&c.chrome)
        });
    let sidebar = sidebar_cfg.as_ref().and_then(|c| {
        sidebar_reservation(
            viewport_dims.0,
            c.enabled,
            c.width,
            match c.position {
                SidebarPosition::Right => SidebarEdge::Right,
                SidebarPosition::Left => SidebarEdge::Left,
            },
            headless_chrome.min_pane_cols,
        )
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
    // phux-i0e8.2.2: headless composite dispatches no kill actions, so the
    // expected-close set stays empty; threaded for the shared signature.
    let mut expected_closes: HashSet<TerminalId> = HashSet::new();

    // Replay ATTACHED so the focused-pane + workspace bootstrap runs once.
    let outcome = handle_server_frame(
        &mut engine_kernel,
        &mut kernel_effects,
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
        &mut expected_closes,
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

    // A rendered snapshot is valid only after the server's aggregate barrier
    // and all work it unlocked has drained. ATTACH_READY can be queued before
    // the requested post-READY history pages or one-shot metadata replies.
    let mut completion = HeadlessCompletion::new(layout_get_request_id);
    tokio::time::timeout(ATTACH_READY_DEADLINE, async {
        loop {
            let frame = conn.recv().await?;
            completion.observe_frame(&frame, attach_id);
            let mut outcome = handle_server_frame(
                &mut engine_kernel,
                &mut kernel_effects,
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
                &mut expected_closes,
                &mut agent_meta,
                false,
                true,
            )?;
            send_terminal_replies(
                &mut conn,
                take_terminal_replies(&mut outcome, terminal_reply_supported),
            )
            .await?;
            if outcome.resync_required {
                if session_name.is_empty() {
                    return Err(AttachError::Protocol(
                        "engine requested rebootstrap before ATTACHED named the session".to_owned(),
                    ));
                }
                attach_id =
                    send_attach(&mut conn, AttachTarget::ByName(session_name.clone())).await?;
                completion.restart_attach();
                continue;
            }
            if let Some((
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                max_bytes,
                max_rows,
            )) = outcome.history_request
            {
                completion.note_history_request(&terminal_id, stream_id, bootstrap_id);
                conn.send(&FrameKind::HistoryRequest {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    cursor,
                    max_bytes,
                    max_rows,
                })
                .await?;
            }
            if completion.is_complete(agent_meta.pending.is_empty()) {
                break;
            }
        }
        Ok::<(), AttachError>(())
    })
    .await
    .map_err(|_| {
        AttachError::Protocol(format!(
            "headless attach {attach_id} timed out before ATTACH_READY, history, and metadata completed"
        ))
    })??;

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
    sidebar_painter.set_needs_you(agent_entries(&workspace, &panes, &agent_meta));

    // Compose the assembled frame against the render layout (honoring zoom).
    let layout_state = workspace.render_window(zoomed.as_ref()).map_or_else(
        crate::layout::LayoutState::default,
        std::borrow::Cow::into_owned,
    );
    let frame = super::rendered::compose_full_frame_cells(
        &layout_state,
        &mut panes,
        &engine_kernel,
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

/// The attach session body shared by the production
/// ([`run_with_predict_dial`]) and test-injectable
/// ([`run_with_stdout_predict`]) entry points.
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
#[allow(
    clippy::too_many_arguments,
    reason = "per-invocation knobs from the run_* entry points; a builder for one internal fn would be ceremony"
)]
async fn attach_session<W: super::RenderSink>(
    dial: &Dial,
    target: AttachTarget,
    out: &mut W,
    predict: PredictiveConfig,
    resync: Option<&AtomicBool>,
    mut writer: Option<super::stdout_writer::WriterHandle>,
    probe_default_colors: bool,
    // phux-i0e8.2.3: transient status-bar notice for the FIRST `main_loop`
    // entry only (an in-invocation session switch must not re-show it) —
    // the reconnect loop's "re-attached after server restart".
    initial_notice: Option<Notice>,
    recorder: Option<Rc<RefCell<SessionRecorder>>>,
) -> Result<AttachEnd, AttachError> {
    // STAGE 1 — pre-handshake, on the cooked outer terminal.
    //
    // We deliberately do NOT install RawModeGuard here. If anything in
    // this block fails (no server, refused, signal during connect) the
    // user's terminal stays in its original state and `Err(_)` carries
    // the actionable cause up to the CLI.
    // Attach-handshake timing (info): HELLO -> ATTACH -> ATTACHED. The
    // span's CLOSE duration is the end-to-end attach latency a trace reader
    // wants for "why was the first paint slow." Lifecycle-rate, so info.
    let handshake_span = tracing::info_span!("attach_handshake", ?target);
    let (mut conn, attached, output_mode) = async {
        let default_colors = probe_default_colors
            .then(super::terminal_probe::default_colors)
            .flatten();
        let client_caps = attach_client_caps(default_colors);
        let conn =
            Connection::connect_dial_with_hello(dial, attach_client_name(), client_caps).await?;
        let negotiated = conn.negotiated_bootstrap().ok_or_else(|| {
            AttachError::Protocol(
                "production connection returned before bootstrap negotiation".to_owned(),
            )
        })?;
        let output_mode = if matches!(
            negotiated.profile,
            phux_protocol::BootstrapProfile::SynthesizedVtStateSync
        ) {
            OutputMode::StateSync
        } else {
            OutputMode::Raw
        };
        let mut conn = conn;
        let attach_id = send_attach(&mut conn, target).await?;
        let attached = wait_for_attached(&mut conn, attach_id).await?;
        Ok::<_, AttachError>((conn, attached, output_mode))
    }
    .instrument(handshake_span)
    .await?;
    // The output mode is a per-connection HELLO property; construction
    // negotiates exactly once and the re-attach loop below reuses the same
    // `conn`, so this bool is stable across an in-connection session switch.
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
    // Register the fatal-signal handler BEFORE raw mode is entered. The
    // handler snapshots termios at install time, so installing it after
    // `RawModeGuard` would capture the *raw* flags and "restore" the user
    // into raw mode on crash — the exact wedge it exists to prevent.
    //
    // This arms the termios-only variant; the escape-code resets are added
    // by `RawModeGuard` once the alt screen is actually up, so a crash
    // before that point does not emit stray DECSETs to a normal screen.
    phux_crash::install_terminal_restore_only();

    let _guard = RawModeGuard::install_with_stdout(out, mouse_capture)?;

    // Install a panic hook so an unexpected panic inside `main_loop`
    // (renderer bug, libghostty FFI surprise, etc.) still restores the
    // terminal before the default hook prints its backtrace. The hook
    // is global, so we only register it once per process.
    //
    // The hook covers panics only — those unwind, so `Drop` and the hook
    // both run. A *fatal* signal (SIGSEGV/SIGBUS/SIGABRT) does neither,
    // which is why `phux_crash` is armed separately above. Our own code is
    // `#![forbid(unsafe_code)]`, so that path is reachable essentially only
    // through the native libghostty-vt FFI boundary — the "FFI surprise"
    // this comment already anticipated, in the one form the hook can't see.
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
    // First-use guidance is profile-scoped, versioned, and best-effort. The
    // moment is decided once per process attach and consumed by the first loop
    // entry, so in-process session switches never repeat it.
    let onboarding_path = super::onboarding::state_path();
    let mut onboarding_claim = super::onboarding::begin_attach(&onboarding_path);
    // phux-foz.8: window index to select after a one-step cross-session
    // window pick (`switch-session { name, window }`) re-attaches. `None`
    // on the first attach and after plain switches; set per-iteration by
    // the SwitchTo arm below, consumed by `main_loop` once the target's
    // persisted layout loads. phux-jpqd: `pending_pane` is the pane half
    // of a one-step cross-session pane pick (`switch-session { .., pane }`).
    let mut pending_window: Option<usize> = None;
    let mut pending_pane: Option<usize> = None;
    // phux-i0e8.2.3: hand the reconnect notice to the first `main_loop`
    // entry only (same `take` pattern as the onboarding hint above): a
    // session switch re-enters `main_loop` but is not a reconnect.
    let mut initial_notice = initial_notice;
    loop {
        let claim = onboarding_claim.take();
        let exit = match main_loop(
            &mut conn,
            attached,
            predict,
            out,
            resync,
            wants_state_sync,
            claim,
            initial_notice.take(),
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
            LoopExit::Detached {
                end,
                locally_requested,
            } => {
                // Lifecycle transition (info): the attach loop is exiting.
                tracing::info!(?end, "attach loop: DETACHED; exiting");
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
                exit_after_detach(end, locally_requested, &onboarding_path, recorder.as_ref());
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
                let attach_id = send_attach(&mut conn, attach_target).await?;
                attached = wait_for_attached(&mut conn, attach_id).await?;
                tracing::info!("attach loop: re-attach handshake complete");
                // Re-enter `main_loop`, which rebuilds ALL session-scoped
                // state fresh (pane mirrors, workspace, predict, overlays,
                // pending-spawn maps, layout subscription) from the new
                // ATTACHED frame, then repaints. A full repaint of the new
                // session's grid happens via the replayed negotiated bootstrap
                // frames inside the loop.
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
            // Any reason ends the drain: we asked for this detach, and the
            // next ATTACH rebuilds every session-scoped thing the reason
            // could have qualified.
            FrameKind::Detached { .. } => return Ok(()),
            other => {
                tracing::trace!(kind = ?other, "draining frame during session switch");
            }
        }
    }
}

/// phux-eb0: clear the alt screen between sessions so the previous
/// session's grid doesn't briefly show under the new session's first
/// paint. The new bootstrap repaint lands immediately after, so this is a
/// one-frame clear, not a flicker.
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
    ack: Option<(
        TerminalId,
        phux_protocol::StreamId,
        phux_protocol::BootstrapId,
        u64,
    )>,
) -> Option<(
    TerminalId,
    phux_protocol::StreamId,
    phux_protocol::BootstrapId,
    u64,
)> {
    wants_state_sync.then_some(ack).flatten()
}

fn take_terminal_replies(
    outcome: &mut FrameOutcome,
    terminal_reply_supported: bool,
) -> Vec<(TerminalId, Vec<u8>)> {
    // phux-501l (hardening, not the fix — see `peer_gone` for that): an
    // outcome that ends the attach loop should not put another byte on the
    // wire. A terminal reply is addressed to a pane's PTY, and once an outcome
    // carries `exit` there is no pane left to route it to.
    //
    // This matters because the call site sends replies BEFORE it inspects
    // `outcome.exit`, so an outcome carrying both would write into a session
    // it is in the middle of abandoning. No current handler produces that
    // combination — the last-pane-closed branch returns a `FrameOutcome` with
    // `pty_writes` defaulted empty — so this closes a latent hole rather than
    // an observed one. It is cheap and it makes the ordering at the call site
    // stop mattering.
    if outcome.exit {
        outcome.pty_writes.clear();
        return Vec::new();
    }
    if terminal_reply_supported {
        return std::mem::take(&mut outcome.pty_writes);
    }
    if outcome.pty_writes.is_empty() {
        return Vec::new();
    }
    outcome.pty_writes.clear();
    let message = "terminal query reply not sent: server lacks terminal-reply support";
    tracing::warn!(feature = ?ServerFeature::TerminalReply, "{message}");
    outcome.notices.push(Notice::warn(message));
    Vec::new()
}

async fn send_terminal_replies(
    conn: &mut Connection,
    replies: Vec<(TerminalId, Vec<u8>)>,
) -> Result<(), AttachError> {
    for (terminal_id, bytes) in replies {
        send_unless_peer_gone(
            conn,
            &FrameKind::InputTerminalReply {
                terminal_id,
                bytes: bytes::Bytes::from(bytes),
            },
        )
        .await?;
    }
    Ok(())
}

/// Whether a write failed because the peer had already closed the connection.
///
/// `BrokenPipe` is the local half discovering the socket is gone;
/// `ConnectionReset`/`ConnectionAborted` are the same discovery on the
/// transports where the kernel reports it that way. None of them describe a
/// fault in *this* process — they describe a peer that is no longer there.
fn peer_gone(err: &AttachError) -> bool {
    matches!(
        err,
        AttachError::Io(inner)
            if matches!(
                inner.kind(),
                io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
            )
    )
}

/// Send a frame, treating "the peer already hung up" as success.
///
/// phux-501l. THE READ SIDE OWNS THE ENDING. A failed write tells us only that
/// the connection is gone; the reason a session ended is carried by frames the
/// server sent *before* it went, and those are already sitting in our decode
/// buffer. Failing the attach loop on the write throws that reason away and
/// replaces it with the mechanical symptom.
///
/// Concretely, the bug this fixes: the last pane's shell exits, so the server
/// emits `TERMINAL_OUTPUT` (the shell's final bytes) immediately followed by
/// `TERMINAL_CLOSED`, then reaps the now-empty session and exits, closing the
/// UDS. The client's next read pulls BOTH frames into one coalesced batch. It
/// processes the `TERMINAL_OUTPUT` first and answers it with a `FRAME_ACK` — a
/// write, into a socket the server has already closed. That returns EPIPE, `?`
/// turns it into `AttachError::Io`, and the loop dies **without ever
/// processing the `TERMINAL_CLOSED` sitting in the same batch**. The user typed
/// `exit 7` and got
///     phux: attach failed: attach loop io error: Broken pipe (os error 32)
/// instead of "session ended: the last pane exited 7".
///
/// Whether the write lost that race was pure scheduling, which is why it read
/// as a flake — nextest's retries had been hiding it on `main`, and it failed
/// 6/6 on the runners where the server won.
///
/// Swallowing the error loses nothing. Every write in the inbound arm is
/// either advisory (`FRAME_ACK` is flow-control accounting the server drops
/// outright for a raw consumer) or a request whose answer can no longer
/// arrive. In both cases the loop continues, drains the frames it already
/// holds, and ends for the reason those frames give: `LastPaneClosed` here, or
/// `AttachError::Disconnected` from the EOF on the following `recv` if the
/// batch carried no ending. Both are strictly better than `Io`, and neither
/// can mask a live-server fault — if the server were still there, the write
/// would not have failed.
async fn send_unless_peer_gone(
    conn: &mut Connection,
    frame: &FrameKind,
) -> Result<(), AttachError> {
    match conn.send(frame).await {
        Ok(()) => Ok(()),
        Err(err) if peer_gone(&err) => {
            tracing::debug!(
                ?err,
                "write dropped: peer already closed; letting the read side name the ending",
            );
            Ok(())
        }
        Err(err) => Err(err),
    }
}
/// Build the reference TUI's per-connection HELLO profile.
///
/// The same value is passed to [`Connection::connect_dial_with_hello`] before
/// any ATTACH can be sent. Keeping it as data rather than a second handshake
/// routine prevents reconnect and custom-capability paths from double-HELLO.
fn attach_client_caps(
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) -> ClientCapabilities {
    // Sniff `$COLORTERM` / `$TERM` / `$TERM_PROGRAM` per
    // `detect_color_support`. The advertised tier feeds the server's
    // per-client `downsample::rewrite_bytes` (SPEC §6.2).
    //
    // phux-4li.5: declare L3 (`Layer::L3`) so the server forwards
    // `MetadataChanged` events for the `phux.tui.layout/v1` key.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    let bootstrap = phux_client_core::engine::ghostty::native_bootstrap_capabilities(
        BootstrapLimits::default(),
    );
    #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
    let bootstrap = BootstrapCapabilities::new().with_limits(BootstrapLimits::default());
    let mut client_caps = ClientCapabilities::new()
        .with_bootstrap(bootstrap)
        .with_color_support(detect_color_support())
        .with_layers(LayerSet::with(&[Layer::L3]));
    if let Some(colors) = default_colors {
        client_caps = client_caps.with_default_colors(colors);
    }
    client_caps
}

fn attach_client_name() -> String {
    format!("phux-client/{}", env!("CARGO_PKG_VERSION"))
}

/// Send the `ATTACH` frame using the current terminal viewport.
async fn send_attach(conn: &mut Connection, target: AttachTarget) -> Result<u32, AttachError> {
    let viewport = current_viewport()?;
    let attach_id = conn.next_attach_id();
    conn.send(&FrameKind::Attach {
        attach_id,
        target,
        viewport,
        // SPEC §13: clients SHOULD opt in to scrollback. The cap below
        // matches the default in docs/consumers/tui.md §X; a configurable knob lives
        // with the rest of `phux-config`.
        request_scrollback: true,
        scrollback_limit_lines: 10_000,
    })
    .await?;
    Ok(attach_id)
}

/// Read frames off `conn` until we get the expected `ATTACHED` reply,
/// surfacing a structured `Error` frame as `AttachError::Refused` and
/// any other unexpected frame as `AttachError::Protocol`.
///
/// Runs entirely on the cooked terminal (pre-`RawModeGuard`) per
/// `phux-roz`. A server-side reject prints an actionable error on the
/// normal screen and exits without flicker.
async fn wait_for_attached(
    conn: &mut Connection,
    expected_attach_id: u32,
) -> Result<FrameKind, AttachError> {
    let frame = conn.recv().await?;
    match frame {
        FrameKind::Attached { attach_id, .. } if attach_id == expected_attach_id => Ok(frame),
        FrameKind::Attached { attach_id, .. } => Err(AttachError::Protocol(format!(
            "ATTACHED attach_id mismatch: sent {expected_attach_id}, received {attach_id}",
        ))),
        FrameKind::Error {
            code: _, message, ..
        } => Err(AttachError::Refused(message)),
        _ => {
            // Anything else this early is a protocol violation. The
            // server is required to answer `ATTACH` with either
            // `ATTACHED` or `ERROR`; reject otherwise rather than
            // silently soldiering on into a half-attached state.
            Err(AttachError::Protocol(crate::explain::unexpected_reply(
                "ATTACH",
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
/// * `Detached(end)` — the user detached, the server sent `DETACHED`, or
///   the last pane closed (phux-i0e8.2.2 — `end` carries which, plus the
///   dead pane's exit status). The outer loop runs `exit_after_detach(end)`
///   on this path, which prints the last-pane explanation on the cooked
///   terminal and exits the process.
/// * `SwitchTo(target)` — the user committed `switch-session { name }`
///   (via the `<leader> a` picker / palette) or `new-session`. The outer
///   loop detaches from the current session and re-runs the handshake
///   against the target (`ByName` for an existing session, `CreateIfMissing`
///   for a new one), then re-enters `main_loop` with the new ATTACHED frame
///   and freshly-rebuilt session state.
#[derive(Debug)]
enum LoopExit {
    /// The session ended (detach / server DETACHED / last pane closed).
    /// Carries WHY (phux-i0e8.2.2) so the teardown path can explain a
    /// last-pane death on the cooked terminal. The process exits.
    Detached {
        end: AttachEnd,
        locally_requested: bool,
    },
    /// Re-attach on the same connection — to an existing session or a
    /// newly-created one.
    SwitchTo(ReattachTarget),
}

/// Classify a detach independently from the wire frame that completed it.
/// A server `DETACHED` is local only when this client had already requested
/// detach; pane death remains its own ending even if the events race.
const fn is_local_detach(end: AttachEnd, local_intent: bool) -> bool {
    local_intent && matches!(end, AttachEnd::Detached { .. })
}

const fn detached_loop_exit(end: AttachEnd, local_intent: bool) -> LoopExit {
    LoopExit::Detached {
        end,
        locally_requested: is_local_detach(end, local_intent),
    }
}

fn finish_onboarding_claim(claim: Option<super::onboarding::AttachClaim>, delivery_accepted: bool) {
    if delivery_accepted && let Some(claim) = claim {
        let _ = claim.commit();
    }
}

fn finish_return_onboarding_after_paint(
    claim: &mut Option<super::onboarding::AttachClaim>,
    status_bar: Option<&StatusBarPainter>,
    paint: StatusBarPaint,
) {
    if paint.delivered(status_bar, super::onboarding::RETURN_NOTICE) {
        finish_onboarding_claim(claim.take(), true);
    }
}

/// Drive the `tokio::select!` loop until detach or a session switch.
///
/// `initial_attached` is the `FrameKind::Attached` frame that
/// [`wait_for_attached`] already pulled off the wire; we replay it
/// through `handle_server_frame` so the focused-pane bookkeeping lives
/// in one place. Subsequent bootstrap and `TERMINAL_OUTPUT` frames come off the
/// wire as usual.
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
    // First-use moment consumed by this loop entry. Session switches receive
    // `None`, so they never repeat attach guidance.
    mut onboarding_claim: Option<super::onboarding::AttachClaim>,
    // phux-i0e8.2.3: transient status-bar notice to seed at attach time —
    // the reconnect loop's "re-attached after server restart". Applied to
    // the painter right after the bootstrap chrome refresh, so the first
    // bar paint (driven by the initial TERMINAL_SNAPSHOT burst) shows it;
    // expiry rides the ordinary 1 s status_tick. `None` on a first attach
    // and on session switches.
    initial_notice: Option<Notice>,
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
    let onboarding_moment = onboarding_claim
        .as_ref()
        .map_or(super::onboarding::AttachMoment::None, |claim| {
            claim.moment()
        });
    // phux-4li.4: hold N client-side Terminals keyed by `TerminalId`,
    // not the single Terminal of the wave-A driver. Each pane's metadata slot
    // is allocated lazily from authoritative bootstrap geometry.
    let negotiated = conn.negotiated_bootstrap().ok_or_else(|| {
        AttachError::Protocol("attach loop started before bootstrap negotiation".to_owned())
    })?;
    let terminal_reply_supported = negotiated
        .server_features
        .contains(ServerFeature::TerminalReply);
    // phux-a5xj: does this server build a spawned pane at the geometry we
    // name, rather than at its own default? Fixed for the life of the
    // connection, like the reply bit above.
    let spawn_initial_size_supported = negotiated
        .server_features
        .contains(ServerFeature::SpawnInitialSize);
    let history_config = HistoryCacheConfig {
        request_max_bytes: negotiated.limits.max_history_page_bytes(),
        ..HistoryCacheConfig::default()
    };
    let mut engine_kernel = SessionKernel::with_history_config(
        GhosttyAdapter::new(negotiated.limits),
        negotiated.profile,
        history_config,
    );
    let mut kernel_effects = KernelEffectBuffer::new();
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
    // Cache the keybindings so opening discovery surfaces never performs
    // config I/O under user fingers. Load the config once and split out the
    // keybindings snapshot and color theme; failures fall back to defaults.
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
    // phux-i0e8.3.4: the build is lenient — whenever a snapshot exists a
    // resolver exists, and each diagnostic disables exactly one binding.
    // Diagnostics surface as a status-bar error line naming the chord
    // (unless the bar is already showing a config error, which subsumes
    // any keybinding problem).
    let mut resolver: Option<phux_config::keybind::Resolver> = None;
    if let Some(kb) = keybindings_snapshot.as_ref() {
        let (built, diags) = build_resolver_from(kb);
        resolver = Some(built);
        if !diags.is_empty()
            && !status_bar
                .as_ref()
                .is_some_and(StatusBarPainter::is_error_line)
        {
            status_bar = Some(StatusBarPainter::error_line(keybind_error_line(&diags)));
        }
    }
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
    // phux-huhi: the responsive-chrome thresholds, snapshotted from
    // `[chrome]` beside the theme and the sidebar geometry. One value for the
    // whole attach: the sidebar-yield fold below, the `toggle-sidebar`
    // refusal in `input_dispatch`, and every overlay's `centered_panel` read
    // this, so "compact" cannot mean two things on the same frame.
    let mut chrome_breakpoints = loaded_cfg
        .as_ref()
        .map_or_else(ChromeBreakpoints::default, |c| {
            ChromeBreakpoints::from_cfg(&c.chrome)
        });
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
    // phux-huhi: stamp the configured breakpoints once, before anything can
    // be pushed. `OverlayState::push` hands them to each overlay from here,
    // so no overlay construction site names a threshold.
    overlays.set_breakpoints(chrome_breakpoints);
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
    // comes from the same `[keybindings]` snapshot the action finder uses;
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
    // phux-i0e8.2.2: Terminals whose close THIS client requested
    // (kill-pane / kill-window). The action dispatcher parks ids here at
    // the kill seam; the `TerminalClosed` arm drains them to suppress the
    // pane-exit notice for a death the user themselves ordered.
    let mut expected_closes: HashSet<TerminalId> = HashSet::new();

    // Replay the `ATTACHED` frame so the focused-pane bookkeeping in
    // `handle_server_frame` runs exactly once, in one place. The sidebar
    // reservation for this bootstrap frame (recomputed per-iteration in the
    // loop below to track `toggle-sidebar`).
    let sidebar = sidebar_reservation(
        viewport_dims.0,
        sidebar_enabled,
        sidebar_width,
        sidebar_edge,
        chrome_breakpoints.min_pane_cols,
    );
    let outcome = handle_server_frame(
        &mut engine_kernel,
        &mut kernel_effects,
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
        &mut expected_closes,
        &mut agent_meta,
        overlays.is_active(),
        // Single replayed frame — no burst to coalesce, paint it.
        false,
    )?;
    if outcome.exit {
        let end = outcome
            .exit_reason
            .unwrap_or(AttachEnd::Detached { reason: None });
        return Ok(detached_loop_exit(end, false));
    }
    // phux-e9fd: size every bootstrap pane's PTY to the rect this client will
    // actually paint it into, before anything else runs.
    //
    // The server sizes each pane from the ATTACH viewport
    // (`apply_attach_viewport`), which is the client's OUTER terminal —
    // chrome included. The client paints panes into `content_rect`, which is
    // one row shorter whenever a status bar is docked. Without this call the
    // mirror is a row taller than the rect it is clipped into, so the pane's
    // bottom line is never painted and the bar looks like it overwrote it.
    // The self-heal users notice — resize, split, toggle the sidebar — is
    // just the first reflow that DID emit `TERMINAL_RESIZE`.
    //
    // The server side already defers the off-by-one here in as many words
    // ("the client's concern via the post-attach `TERMINAL_RESIZE` reflow
    // path"); this is that path, and until now nothing called it. An empty
    // `prev_rects` makes `compute_reflow` report every leaf as changed — its
    // documented first-attach rule — so each pane is sized exactly once.
    emit_view_reflow(
        conn,
        &workspace,
        zoomed.as_ref(),
        &HashMap::new(),
        content_rect(
            viewport_dims,
            status_bar.as_ref().map(StatusBarPainter::position),
            sidebar,
        ),
    )
    .await?;
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
    // the first bootstrap-driven bar paint shows the window.
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

    // phux-i0e8.2.3: seed the post-reconnect notice now that the session is
    // attached and the bar painter exists. The first bar paint — driven by
    // the initial TERMINAL_SNAPSHOT burst that follows ATTACHED — picks it
    // up, and the ordinary 1 s status_tick expires it, so "re-attached
    // after server restart" is visible inside the live TUI instead of on
    // the cooked terminal the alt screen replaced.
    let return_notice_available =
        initial_notice.is_none() && onboarding_moment == super::onboarding::AttachMoment::Return;
    let initial_notice = initial_notice.or_else(|| {
        return_notice_available.then(|| Notice::info(super::onboarding::RETURN_NOTICE))
    });
    let notice_accepted = apply_initial_notice(status_bar.as_mut(), initial_notice);
    if onboarding_moment == super::onboarding::AttachMoment::Return
        && (!return_notice_available || !notice_accepted)
    {
        onboarding_claim.take();
    }

    // The introduction floats over the live pane after bootstrap. It is a
    // passthrough notice: the first key dismisses it and continues through the
    // normal resolver/pane route, so guidance never taxes the user's intent.
    if onboarding_moment == super::onboarding::AttachMoment::Intro {
        overlays.push(Box::new(crate::render::overlay::ToastOverlay::passthrough(
            super::onboarding::ONBOARDING_TITLE,
            super::onboarding::hint_lines(keybindings_snapshot.as_ref()),
            &theme,
        )));
        paint_active_overlay(
            out,
            &overlays,
            &workspace,
            &mut panes,
            &engine_kernel,
            focused_pane.as_ref(),
            zoomed.as_ref(),
            viewport_dims,
            status_bar.as_mut(),
            sidebar,
            Some(&mut sidebar_painter),
            &session_name,
            &theme,
        );
        let paint_accepted = out.flush().is_ok();
        finish_onboarding_claim(onboarding_claim.take(), paint_accepted);
    }

    loop {
        // phux-4h5a: fold the driver-local sidebar render state into the
        // per-frame reservation threaded to every layout site this iteration.
        // `toggle-sidebar` flips `sidebar_enabled`; the change takes effect on
        // the next iteration. `None` (the default) keeps `content_rect` the
        // full pane viewport, so the whole path is byte-identical when the
        // sidebar is off.
        let sidebar = sidebar_reservation(
            viewport_dims.0,
            sidebar_enabled,
            sidebar_width,
            sidebar_edge,
            chrome_breakpoints.min_pane_cols,
        );
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
                let painted = paint_chrome_in_place(
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
                finish_return_onboarding_after_paint(
                    &mut onboarding_claim,
                    status_bar.as_ref(),
                    painted,
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
                let painted = paint_active_overlay(
                    out,
                    &overlays,
                    &workspace,
                    &mut panes,
                    &engine_kernel,
                    focused_pane.as_ref(),
                    zoomed.as_ref(),
                    viewport_dims,
                    status_bar.as_mut(),
                    sidebar,
                    Some(&mut sidebar_painter),
                    &session_name,
                    &theme,
                );
                finish_return_onboarding_after_paint(
                    &mut onboarding_claim,
                    status_bar.as_ref(),
                    painted,
                );
            } else if let Some(ls) = workspace.render_window(zoomed.as_ref()).as_deref() {
                let painted = paint_full_frame(
                    out,
                    ls,
                    &mut panes,
                    &engine_kernel,
                    focused_pane.as_ref(),
                    viewport_dims,
                    status_bar.as_mut(),
                    sidebar,
                    Some(&mut sidebar_painter),
                    &session_name,
                );
                finish_return_onboarding_after_paint(
                    &mut onboarding_claim,
                    status_bar.as_ref(),
                    painted,
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
                let sidebar_targets = sidebar_painter.click_targets();
                let mut ctx = DispatchCtx {
                    engine_kernel: &mut engine_kernel,
                    resolver: resolver.as_mut(),
                    focus_history: focus_history.clone(),
                    workspace: &mut workspace,
                    viewport: viewport_dims,
                    cell_px: cell_px_dims,
                    next_request_id: &mut next_request_id,
                    spawn_initial_size_supported,
                    pending_splits: &mut pending_splits,
                    pending_windows: &mut pending_windows,
                    expected_closes: &mut expected_closes,
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
                    sidebar_width,
                    chrome: chrome_breakpoints,
                    sidebar_targets: &sidebar_targets,
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
                let sidebar = sidebar_reservation(viewport_dims.0, sidebar_enabled, sidebar_width, sidebar_edge, chrome_breakpoints.min_pane_cols);
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
                        let painted = paint_full_frame(
                            out,
                            ls,
                            &mut panes,
                            &engine_kernel,
                            focused_pane.as_ref(),
                            viewport_dims,
                            status_bar.as_mut(),
                            sidebar,
                            Some(&mut sidebar_painter),
                            &session_name,
                        );
                        finish_return_onboarding_after_paint(
                            &mut onboarding_claim,
                            status_bar.as_ref(),
                            painted,
                        );
                    }
                }
                if overlays.is_active() {
                    let painted = paint_active_overlay(
                        out,
                        &overlays,
                        &workspace,
                        &mut panes,
                        &engine_kernel,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                        &theme,
                    );
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
                    );
                }
                // phux-foz.5: a `reload-config` committed in this batch
                // (palette row or bound chord). Runs LAST in the arm so
                // its repaint reflects the new theme/bar.
                if reload_request {
                    reload_request = false;
                    let painted = handle_config_reload(
                        out,
                        &mut keybindings_snapshot,
                        &mut resolver,
                        &mut theme,
                        &mut chrome_breakpoints,
                        &mut status_bar,
                        &mut sidebar_painter,
                        &mut plugin_actions,
                        &mut plugin_panes,
                        &mut which_key_enabled,
                        &mut which_key_delay,
                        &mut overlays,
                        &workspace,
                        &mut panes,
                        &engine_kernel,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        own_client_id,
                        &agent_meta,
                        &mut vcs,
                        viewport_dims,
                        sidebar,
                        &session_name,
                    );
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
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
                        let mut outcome = handle_server_frame(
                            &mut engine_kernel,
                            &mut kernel_effects,
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
                            &mut expected_closes,
                            &mut agent_meta,
                            overlays.is_active(),
                            defer_paint,
                        )?;
                        send_terminal_replies(
                            conn,
                            take_terminal_replies(&mut outcome, terminal_reply_supported),
                        )
                        .await?;
                        focus_history.observe(focused_before_frame, focused_pane.as_ref());
                        focus_history.repair(focused_pane.as_ref(), &workspace);
                        if outcome.exit {
                            let end = outcome.exit_reason.unwrap_or(AttachEnd::Detached { reason: None });
                            return Ok(detached_loop_exit(end, detach_pending));
                        }
                        if outcome.resync_required {
                            if session_name.is_empty() {
                                return Err(AttachError::Protocol(
                                    "engine requested rebootstrap before ATTACHED named the session"
                                        .to_owned(),
                                ));
                            }
                            let attach_id =
                                send_attach(conn, AttachTarget::ByName(session_name.clone())).await?;
                            tracing::warn!(
                                attach_id,
                                session = %session_name,
                                "engine generation rejected; requested replacement bootstrap"
                            );
                            continue;
                        }
                        // A peer headless placement can add a layout leaf
                        // without this attached client being subscribed to the
                        // new Terminal. Attach each discovered leaf so its
                        // snapshot creates a PaneSlot and renders in place.
                        for terminal_id in &outcome.attach_panes {
                            let request_id = next_request_id;
                            next_request_id = next_request_id.wrapping_add(1);
                            send_unless_peer_gone(conn, &FrameKind::Command {
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
                        finish_return_onboarding_after_paint(
                            &mut onboarding_claim,
                            status_bar.as_ref(),
                            outcome.status_bar_painted,
                        );
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
                        // phux-i0e8.2.1: drain the frame's transient notices
                        // into the painter's newest-wins slot; expiry rides
                        // the 1 s status_tick arm below. With no bar to paint
                        // on (no painter, an empty bar, or the persistent
                        // error line holding the row — the painter refuses
                        // those itself) the notice degrades to a tracing
                        // line rather than vanishing.
                        if !outcome.notices.is_empty() {
                            let now = std::time::Instant::now();
                            let mut notice_shown = false;
                            for notice in outcome.notices {
                                if let Some(sb) = status_bar.as_mut() {
                                    notice_shown |= sb.set_notice(notice, now);
                                } else {
                                    tracing::info!(
                                        severity = ?notice.severity,
                                        text = %notice.text,
                                        "status-bar notice dropped: no status bar configured",
                                    );
                                }
                            }
                            if notice_shown && !overlays.is_active() {
                                repaint.raise_chrome();
                            }
                        }
                        if let Some((terminal_id, stream_id, bootstrap_id, seq)) =
                            should_emit_frame_ack(wants_state_sync, outcome.ack)
                        {
                            send_unless_peer_gone(conn, &FrameKind::FrameAck {
                                terminal_id,
                                stream_id,
                                bootstrap_id,
                                seq,
                            })
                            .await?;
                        }
                        if let Some((
                            terminal_id,
                            stream_id,
                            bootstrap_id,
                            cursor,
                            max_bytes,
                            max_rows,
                        )) = outcome.history_request
                        {
                            send_unless_peer_gone(conn, &FrameKind::HistoryRequest {
                                terminal_id,
                                stream_id,
                                bootstrap_id,
                                cursor,
                                max_bytes,
                                max_rows,
                            })
                            .await?;
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
                            send_unless_peer_gone(conn, &FrameKind::SetMetadata {
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
                                send_unless_peer_gone(conn, &FrameKind::TerminalResize {
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
                            // phux-z6wt: this arm fires for a peer's layout
                            // broadcast and for the TerminalSpawned/
                            // TerminalClosed reflow — neither goes through
                            // SIGWINCH, so the phux-d26y fan-out never ran
                            // for them. A surviving copy-mode overlay would
                            // keep clamping against the pane size it opened
                            // with, silently dropping or clipping a copy.
                            // Recompute the focused pane's rect the same way
                            // the SIGWINCH arm does and hand it to every
                            // surviving overlay before the repaint below.
                            sync_overlays_to_focused_pane(
                                &mut overlays,
                                &workspace,
                                zoomed.as_ref(),
                                focused_pane.as_ref(),
                                viewport_dims,
                                status_bar.as_ref().map(StatusBarPainter::position),
                                sidebar,
                            );
                            // The pane rects moved: only a full-viewport
                            // repaint (ED2 + every pane + dividers) is a
                            // coherent base. ADR-0029: raise, drain once.
                            if !overlays.is_active() {
                                repaint.raise_full();
                            }
                            // The GET reply is single-use; clear the pending
                            // request id so a stray late MetadataValue can't
                            // trample state. Gated on `layout_get_answered`,
                            // NOT on `layout_replaced`: the latter is also
                            // raised for pane damage during bootstrap, and
                            // clearing on that dropped the real reply.
                            if outcome.layout_get_answered {
                                layout_get_request_id = None;
                            }
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
                            let painted = handle_config_reload(
                                out,
                                &mut keybindings_snapshot,
                                &mut resolver,
                                &mut theme,
                                &mut chrome_breakpoints,
                                &mut status_bar,
                                &mut sidebar_painter,
                                &mut plugin_actions,
                                &mut plugin_panes,
                                &mut which_key_enabled,
                                &mut which_key_delay,
                                &mut overlays,
                                &workspace,
                                &mut panes,
                                &engine_kernel,
                                focused_pane.as_ref(),
                                zoomed.as_ref(),
                                own_client_id,
                                &agent_meta,
                                &mut vcs,
                                viewport_dims,
                                sidebar,
                                &session_name,
                            );
                            finish_return_onboarding_after_paint(
                                &mut onboarding_claim,
                                status_bar.as_ref(),
                                painted,
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
                            let painted = refresh_fleet_if_open(
                                out,
                                &mut overlays,
                                &workspace,
                                &mut panes,
                                &engine_kernel,
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
                            finish_return_onboarding_after_paint(
                                &mut onboarding_claim,
                                status_bar.as_ref(),
                                painted,
                            );
                        }
                        if !overlays.is_active()
                            && let Some(ls) = workspace.render_window(zoomed.as_ref()).as_deref()
                        {
                            let status_bar_painted = match drained.level {
                                RepaintLevel::None => StatusBarPaint::NotPublished,
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
                                    &engine_kernel,
                                    focused_pane.as_ref(),
                                    viewport_dims,
                                    status_bar.as_mut(),
                                    sidebar,
                                    Some(&mut sidebar_painter),
                                    &session_name,
                                ),
                            };
                            finish_return_onboarding_after_paint(
                                &mut onboarding_claim,
                                status_bar.as_ref(),
                                status_bar_painted,
                            );
                        }
                    }
                    Err(AttachError::Disconnected) if detach_pending => {
                        // Server closed the socket without a `DETACHED`
                        // frame — treat it as a clean shutdown because
                        // the user requested detach. Otherwise the loop
                        // bubbles the disconnect up unchanged. No frame
                        // arrived, so there is no stated reason to carry.
                        return Ok(detached_loop_exit(
                            AttachEnd::Detached { reason: None },
                            true,
                        ));
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
                    let painted = paint_full_frame(
                        out,
                        ls,
                        &mut panes,
                        &engine_kernel,
                        focused_pane.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                    );
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
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
                let sidebar_targets = sidebar_painter.click_targets();
                let mut ctx = DispatchCtx {
                    engine_kernel: &mut engine_kernel,
                    resolver: resolver.as_mut(),
                    focus_history: focus_history.clone(),
                    workspace: &mut workspace,
                    viewport: viewport_dims,
                    cell_px: cell_px_dims,
                    next_request_id: &mut next_request_id,
                    spawn_initial_size_supported,
                    pending_splits: &mut pending_splits,
                    pending_windows: &mut pending_windows,
                    expected_closes: &mut expected_closes,
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
                    sidebar_width,
                    chrome: chrome_breakpoints,
                    sidebar_targets: &sidebar_targets,
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
                let sidebar = sidebar_reservation(viewport_dims.0, sidebar_enabled, sidebar_width, sidebar_edge, chrome_breakpoints.min_pane_cols);
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
                    let painted = paint_full_frame(
                        out,
                        ls,
                        &mut panes,
                        &engine_kernel,
                        focused_pane.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                    );
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
                    );
                }
                if overlays.is_active() {
                    let painted = paint_active_overlay(
                        out,
                        &overlays,
                        &workspace,
                        &mut panes,
                        &engine_kernel,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                        &theme,
                    );
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
                    );
                }
                // phux-foz.5: same reload-on-commit check as the stdin
                // arm — a bare-ESC flush can carry the final chord of a
                // palette selection committing `reload-config`.
                if reload_request {
                    reload_request = false;
                    let painted = handle_config_reload(
                        out,
                        &mut keybindings_snapshot,
                        &mut resolver,
                        &mut theme,
                        &mut chrome_breakpoints,
                        &mut status_bar,
                        &mut sidebar_painter,
                        &mut plugin_actions,
                        &mut plugin_panes,
                        &mut which_key_enabled,
                        &mut which_key_delay,
                        &mut overlays,
                        &workspace,
                        &mut panes,
                        &engine_kernel,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        own_client_id,
                        &agent_meta,
                        &mut vcs,
                        viewport_dims,
                        sidebar,
                        &session_name,
                    );
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
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
                    let painted = paint_active_overlay(
                        out,
                        &overlays,
                        &workspace,
                        &mut panes,
                        &engine_kernel,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                        &theme,
                    );
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
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
                let (predict_cols, predict_rows) = focused_pane
                    .as_ref()
                    .and_then(|fid| panes.get(fid))
                    .map_or((viewport.cols, viewport.rows), |slot| slot.geometry);
                predict.set_viewport(predict_cols, predict_rows);
                conn.send(&viewport_resize_frame(viewport)).await?;

                // Emit one TERMINAL_RESIZE per leaf whose (w, h) actually
                // changed so the server ioctls TIOCSWINSZ on each PTY. This
                // covers the single-pane case too — `Workspace::single` seeds
                // a one-leaf tree, so the `tree.is_some()` guard only skips a
                // workspace with no panes at all, and a lone pane still needs
                // sizing to the chrome-inset content rect.
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
                    // phux-d26y / phux-z6wt: the survivors keep their state
                    // but must adopt the focused pane's NEW size before they
                    // are painted. Copy-mode is the one that cares: it
                    // clamps its cursor and picks Line mode's right edge
                    // from pane dimensions captured when it opened, so
                    // without this a copy after a resize either resolves to
                    // nothing (a stale-large corner the engine cannot
                    // address) or stops at the old edge (stale-small). Runs
                    // after the stale sweep above, so an overlay about to be
                    // dropped is never handed geometry it will not use.
                    // Same choke point the `layout_replaced` arm uses for
                    // the non-SIGWINCH triggers (a peer's layout broadcast,
                    // TerminalSpawned/TerminalClosed reflow).
                    sync_overlays_to_focused_pane(
                        &mut overlays,
                        &workspace,
                        zoomed.as_ref(),
                        focused_pane.as_ref(),
                        viewport_dims,
                        status_bar.as_ref().map(StatusBarPainter::position),
                        sidebar,
                    );
                    let painted = paint_active_overlay(
                        out,
                        &overlays,
                        &workspace,
                        &mut panes,
                        &engine_kernel,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                        &theme,
                    );
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
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
                // phux-i0e8.2.1: expire the transient notice on the tick that
                // carries the bar's repaint cadence. The clear invalidates the
                // painter's cache, so the paint below restores the widget row.
                // Runs even while an overlay is up (the bar repaints on
                // overlay dismiss, and a stale notice must not resurface).
                if let Some(sb) = status_bar.as_mut() {
                    let _ = sb.clear_expired_notice(std::time::Instant::now());
                }
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
                    let painted = paint_bar_after_pane(
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
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
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
                    let painted = paint_active_overlay(
                        out,
                        &overlays,
                        &workspace,
                        &mut panes,
                        &engine_kernel,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                        &theme,
                    );
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
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
    engine_kernel: &AttachKernel,
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
) -> StatusBarPaint {
    if !overlays.is_active() {
        return StatusBarPaint::NotPublished;
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
            engine_kernel,
            focused_pane,
            zoomed,
            viewport_dims,
            status_bar,
            sidebar,
            Some(sidebar_painter),
            session_name,
            theme,
        )
    } else {
        StatusBarPaint::NotPublished
    }
}

/// phux-4li.5: build a [`phux_config::keybind::Resolver`] from a
/// keybindings snapshot (post phux-r82.5: the plugin-merged one, so
/// manifest `keys` chords resolve like user bindings — the merge already
/// validated each contributed chord, so a plugin can't poison this
/// build).
///
/// phux-i0e8.3.4: the build is **lenient per binding** — a resolver
/// always comes back, and each diagnostic disables exactly the binding
/// it names. Before this, one malformed chord failed the whole build and
/// silently disabled EVERY binding, including `detach`. Diagnostics are
/// logged here; the caller surfaces them as a visible status-bar error
/// line ([`keybind_error_line`]). Config reload deliberately stays
/// all-or-nothing instead (`super::reload`, docs/consumers/tui.md §4.3).
fn build_resolver_from(
    kb: &phux_config::KeybindingsCfg,
) -> (
    phux_config::keybind::Resolver,
    Vec<phux_config::keybind::BindingDiagnostic>,
) {
    let (resolver, diagnostics) = phux_config::keybind::Resolver::new_lenient(kb);
    for diag in &diagnostics {
        tracing::warn!(binding = %diag.binding, error = %diag.error, "keybinding disabled");
    }
    (resolver, diagnostics)
}

/// phux-i0e8.3.4: format the lenient resolver's diagnostics as the
/// one-line status-bar error strip. Names the first offending chord, the
/// reason, how many more bindings (if any) were also disabled, and the
/// actionable next step (`phux config check`). Empty input formats to an
/// empty string (callers gate on non-empty diagnostics).
fn keybind_error_line(diags: &[phux_config::keybind::BindingDiagnostic]) -> String {
    let Some(first) = diags.first() else {
        return String::new();
    };
    let more = diags.len() - 1;
    if more == 0 {
        format!(
            "keybinding \"{}\" disabled: {} (run: phux config check)",
            first.binding, first.error
        )
    } else {
        format!(
            "keybinding \"{}\" disabled: {} (+{more} more; run: phux config check)",
            first.binding, first.error
        )
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

/// Snapshot the current workspace as the window widget's input.
///
/// Labels prefer structured agent metadata, then the focused pane's cached
/// OSC 0/2 title, then the stored window name.
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
                .map(|slot| slot.last_title.trim())
                .filter(|title| !title.is_empty())
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
        for (leaf, id) in leaves.iter().enumerate() {
            let asked = panes.get(id).is_some_and(|slot| slot.attention);
            let seen = panes.get(id).is_some_and(|slot| slot.seen);
            let change_at = agent_meta.change_at.get(id).copied();
            let mut push = |entry: AgentEntry| {
                let rank = attention_rank(entry.state, entry.attention, entry.seen);
                rows.push((entry, rank, change_at));
            };
            if let Some(record) = agent_meta.records.get(id) {
                push(AgentEntry {
                    // Local rows: `None` commits the cheap client-local
                    // `select-window` rather than a re-attach (phux-k0cw).
                    session: None,
                    window: i,
                    window_name: w.name.clone(),
                    pane: Some(leaf),
                    name: record.name.clone(),
                    state: record.state,
                    attention: asked || record.effective_attention() == AgentAttention::High,
                    seen,
                });
                continue;
            }
            let title_name = panes
                .get(id)
                .map(|slot| slot.last_title.as_str())
                .and_then(agent_name_from_title);
            if let Some(name) = title_name {
                push(AgentEntry {
                    session: None,
                    window: i,
                    window_name: w.name.clone(),
                    pane: Some(leaf),
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
        // phux-w7z2.57: `phux.agent/v1` does not federate. A hub's metadata
        // store holds nothing for a satellite pane, so the `GET` can only
        // answer "unset" and the server now refuses the `SUBSCRIBE` outright
        // with `ERROR { UNSUPPORTED_SATELLITE_ROUTE }`. Sending them anyway
        // would spend two frames per remote pane to earn a warning notice in
        // the status area on every pane-set change. Deliberately not recorded
        // in `subscribed`: that set means "a live watch exists", and none
        // does — the re-skip costs nothing because no frame is sent either way.
        if !id.is_local() {
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

/// Emit one `TERMINAL_RESIZE` per pane whose dimensions differ between
/// `prev_rects` and the new content view. Reuses the close/SIGWINCH reflow
/// path so each PTY's winsize tracks the on-screen geometry. Sent before
/// repainting, mirroring the other reflow sites.
///
/// Called on a pane-zoom or sidebar toggle with the pre-toggle rects, and once
/// at attach with an empty map — which `compute_reflow` reads as "every leaf is
/// new", seeding each PTY at the rect it is painted into (phux-e9fd).
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
/// the user at `phux config check` for the full diagnostic, instead of
/// dropping to an empty bar with only a `tracing::warn` nobody sees
/// (keybindings degrade separately, per binding — see
/// [`build_resolver_from`]). Returns `None`
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
    chrome: &mut ChromeBreakpoints,
    status_bar: &mut Option<StatusBarPainter>,
    sidebar_painter: &mut SidebarPainter,
    plugin_actions: &mut Vec<PluginActionEntry>,
    plugin_panes: &mut Vec<plugin_panes::PluginPaneEntry>,
    which_key_enabled: &mut bool,
    which_key_delay: &mut Duration,
    overlays: &mut OverlayState,
    workspace: &Workspace,
    panes: &mut HashMap<TerminalId, PaneSlot>,
    engine_kernel: &AttachKernel,
    focused_pane: Option<&TerminalId>,
    zoomed: Option<&TerminalId>,
    own_client_id: Option<ClientId>,
    agent_meta: &AgentMetaIndex,
    vcs: &mut VcsIndex,
    viewport_dims: (u16, u16),
    sidebar: Option<SidebarReservation>,
    session_name: &str,
) -> StatusBarPaint {
    let mut painted = StatusBarPaint::NotPublished;
    match super::reload::reload_in_place(
        &phux_config::loader::config_path(),
        keybindings_snapshot,
        resolver,
        theme,
        chrome,
        status_bar,
        plugin_actions,
        plugin_panes,
        which_key_enabled,
        which_key_delay,
    ) {
        Ok(()) => {
            tracing::info!("config reloaded in place");
            // phux-huhi: the new `[chrome]` thresholds reach the overlay
            // stack immediately, including any modal already open.
            overlays.set_breakpoints(*chrome);
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
                painted = paint_full_frame(
                    out,
                    ls,
                    panes,
                    engine_kernel,
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
                    "Fix the file and reload again (run: phux config check)".to_owned(),
                ],
                theme,
            )));
        }
    }
    if overlays.is_active() {
        painted = paint_active_overlay(
            out,
            overlays,
            workspace,
            panes,
            engine_kernel,
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
    painted
}

/// phux-i0e8.2.3: set a caller-supplied attach-time notice (the reconnect
/// loop's "re-attached after server restart") on the status-bar painter's
/// transient slot.
///
/// Returns `true` when the painter accepted it. Degrades to a `tracing`
/// line — never silently — when there is no painter, mirroring the
/// per-frame `FrameOutcome::notices` drain; the painter itself degrades
/// the empty-bar and persistent-error-line cases the same way inside
/// `set_notice`.
fn apply_initial_notice(status_bar: Option<&mut StatusBarPainter>, notice: Option<Notice>) -> bool {
    let Some(notice) = notice else {
        return false;
    };
    if let Some(sb) = status_bar {
        sb.set_notice(notice, std::time::Instant::now())
    } else {
        tracing::info!(
            severity = ?notice.severity,
            text = %notice.text,
            "attach-time notice dropped: no status bar configured",
        );
        false
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
    let manifests = if cfg.plugins.is_empty() {
        Vec::new()
    } else {
        let config_path = phux_config::loader::config_path();
        phux_config::plugin::load_enabled_manifests(&config_path, &cfg.plugins)
    };
    // phux-i0e8.6.1: the composition itself (plugin `[[widgets]]` merge,
    // bar build, `[status] position`, prefix) is shared with the reload
    // path via `reload::compose_status_bar` so the two cannot drift.
    // Only the error POLICY differs, and it stays here: startup degrades
    // a build failure to the error-line painter so a broken config never
    // blocks attach; a reload instead fails atomically and keeps the
    // previous config.
    match super::reload::compose_status_bar(&cfg, &manifests) {
        Ok(painter) => painter,
        Err(err) => {
            tracing::warn!(error = %err, "status-bar build failed; surfacing on status bar");
            Some(StatusBarPainter::error_line(config_error_line(&err)))
        }
    }
}

/// phux-9vf: format a one-line, on-screen config error for the status
/// bar: the `Display` of the error plus the actionable next step.
/// The remedy is `phux config check` — the verb that diagnoses, with
/// key paths and layer attribution — not `config show`, which only
/// renders the effective config (phux-i0e8.3.5).
fn config_error_line(err: &impl std::fmt::Display) -> String {
    format!("config error: {err} (run: phux config check)")
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
/// kitty-graphics `FALLBACK_CELL_PX` in `pane_state.rs`): with no pixel report
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

        // Upgrade the fatal-signal handler to also emit the DECSET resets.
        // Paired with the matching downgrade in `Drop`, so the escape codes
        // are only ever written while there is genuinely an alt screen and
        // mouse tracking to undo. No-op if the handler was never installed
        // (the writer-injecting test path never reaches here — it returns
        // `NotATty` above — but a future caller might).
        phux_crash::enable_terminal_escape_restore();

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

        // Downgrade the fatal-signal handler back to termios-only. The alt
        // screen is gone; a crash from here on must not spray DECSET resets
        // across the user's restored normal screen. Deliberately last, so a
        // fault *during* the reset above is still covered by the full
        // sequence.
        phux_crash::disable_terminal_escape_restore();
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

/// Best-effort termios restore shared by signal and clean-detach exits.
/// Termios goes back to the saved state (recovered from [`SAVED_TERMIOS`]
/// when populated; otherwise a re-cook fall-back). Errors are swallowed: the
/// process is on its way out.
///
/// Behaviour change for phux-2r7 (was best-effort re-cook only,
/// committed in 63dc6ff): when [`RawModeGuard`] has parked a snapshot,
/// we now do a true `tcsetattr` restore to the user's pre-attach
/// flags, preserving customisations like IUTF8 / VEOF that the re-cook
/// would clobber. The manual SIGINT-during-attach repro that motivated
/// the original fix still passes; verifying the precise-restore
/// behaviour requires a live PTY and is not unit-testable from here.
fn restore_terminal_termios() {
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
}

/// Restore termios and leave the alt screen from a signal handler arm.
fn terminal_reset_on_signal() {
    restore_terminal_termios();
    let mut out = io::stdout().lock();
    let _ = write_terminal_reset(&mut out);
}

fn write_terminal_reset_and_finalize<W: Write>(
    out: &mut W,
    recorder: Option<&Rc<RefCell<SessionRecorder>>>,
) {
    if let Some(recorder) = recorder {
        {
            let mut tee = TeeSink {
                inner: out,
                rec: Rc::clone(recorder),
            };
            let _ = write_terminal_reset(&mut tee);
        }
        if let Err(err) = recorder.borrow_mut().finish_in_place() {
            tracing::warn!(error = %err, "closing the session recording failed");
        }
    } else {
        let _ = write_terminal_reset(out);
    }
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
///
/// phux-i0e8.2.2: because this never returns, the CLI's own `Ok(end)`
/// handling can't run on this path — so the one-line explanation for a
/// last-pane death (`AttachEnd::explanation`) is printed HERE, after the
/// terminal reset (the screen is cooked again) and before the exit. A
/// plain detach explains nothing. Process exit stays `0` either way:
/// the attach succeeded; the ending just deserves words.
#[allow(
    clippy::exit,
    reason = "detach must exit now; runtime drop hangs on the stdin read thread"
)]
#[allow(
    clippy::print_stderr,
    reason = "phux-i0e8.2.2: the terminal is cooked again and the process exits before the CLI could print; this is the only window for the last-pane explanation"
)]
fn exit_after_detach(
    end: AttachEnd,
    locally_requested: bool,
    onboarding_path: &std::path::Path,
    recorder: Option<&Rc<RefCell<SessionRecorder>>>,
) -> ! {
    restore_terminal_termios();
    let mut stdout = io::stdout().lock();
    write_terminal_reset_and_finalize(&mut stdout, recorder);
    drop(stdout);
    if let Some(line) = end.explanation() {
        eprintln!("{line}");
    } else if locally_requested && let Some(line) = super::onboarding::after_detach(onboarding_path)
    {
        eprintln!("{line}");
    }
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
    use phux_protocol::PROTOCOL_VERSION;

    fn published_test_kernel(
        terminal_id: &TerminalId,
        cols: u16,
        rows: u16,
        bytes: &[u8],
    ) -> AttachKernel {
        published_test_state(&[(terminal_id, cols, rows, bytes)]).0
    }
    use phux_protocol::caps::{
        BootstrapCapabilities, ServerCapabilities, TerminalColor, TerminalDefaultColors,
        select_bootstrap_profile,
    };
    use phux_protocol::wire::frame::DetachReason;
    use tokio::net::UnixStream;

    static TERMINAL_RESET_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn detach_classification_requires_local_intent_and_plain_detach() {
        assert!(is_local_detach(AttachEnd::Detached { reason: None }, true));
        assert!(!is_local_detach(
            AttachEnd::Detached { reason: None },
            false
        ));
        // The reason qualifies the ending, never the local-intent test: a
        // server that names REQUESTED for a detach we asked for is the same
        // local detach as a server that names nothing.
        assert!(is_local_detach(
            AttachEnd::Detached {
                reason: Some(DetachReason::Requested),
            },
            true,
        ));
        assert!(!is_local_detach(
            AttachEnd::Detached {
                reason: Some(DetachReason::ServerShutdown),
            },
            false,
        ));
        assert!(!is_local_detach(
            AttachEnd::LastPaneClosed {
                exit_status: Some(0),
            },
            true,
        ));
    }

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

    /// phux-i0e8.2.3: the attach-time notice seam `main_loop` calls right
    /// after the bootstrap chrome refresh. A configured bar accepts the
    /// reconnect notice (and paints it full-row on the next bar paint); no
    /// painter, or no notice, is a quiet no-op.
    #[test]
    fn apply_initial_notice_sets_the_painter_slot_at_attach() {
        use phux_config::widget::WidgetRegistry;
        use phux_config::{StatusCfg, Widget};

        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..StatusCfg::default()
        };
        let bar = phux_config::widget::StatusBar::build(&cfg, &WidgetRegistry::with_builtins())
            .expect("bar");
        let mut painter =
            StatusBarPainter::new(bar, crate::render::chrome::status_bar::Position::Bottom);
        let before = std::time::Instant::now();
        assert!(
            apply_initial_notice(
                Some(&mut painter),
                Some(Notice::info("re-attached after server restart")),
            ),
            "a configured bar must accept the reconnect notice"
        );
        // The slot is genuinely occupied: it survives until NOTICE_TTL and
        // clears on the tick after — the same expiry path the live
        // status_tick drives (full-row rendering itself is pinned by the
        // phux-i0e8.2.1 painter tests).
        assert!(
            !painter.clear_expired_notice(before),
            "the notice must hold the slot for its TTL"
        );
        assert!(
            painter
                .clear_expired_notice(before + crate::render::chrome::status_bar::NOTICE_TTL * 2),
            "the seeded notice must expire like any other transient notice"
        );

        // No painter: degrades (returns false), never panics.
        assert!(!apply_initial_notice(
            None,
            Some(Notice::info("re-attached after server restart")),
        ));
        // No notice: a first attach is a no-op even with a painter.
        assert!(!apply_initial_notice(Some(&mut painter), None));
    }

    fn returning_onboarding_claim(path: &std::path::Path) -> super::super::onboarding::AttachClaim {
        let intro = super::super::onboarding::begin_attach(path).expect("intro claim");
        assert!(intro.commit());
        assert_eq!(
            super::super::onboarding::after_detach(path),
            Some(super::super::onboarding::DETACH_NOTICE)
        );
        super::super::onboarding::begin_attach(path).expect("return claim")
    }

    #[test]
    fn accepted_return_notice_is_retryable_when_attach_exits_before_paint() {
        use phux_config::widget::WidgetRegistry;
        use phux_config::{StatusCfg, Widget};

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("onboarding.json");
        let claim = returning_onboarding_claim(&path);
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..StatusCfg::default()
        };
        let bar = phux_config::widget::StatusBar::build(&cfg, &WidgetRegistry::with_builtins())
            .expect("bar");
        let mut painter =
            StatusBarPainter::new(bar, crate::render::chrome::status_bar::Position::Bottom);

        assert!(apply_initial_notice(
            Some(&mut painter),
            Some(Notice::info(super::super::onboarding::RETURN_NOTICE)),
        ));
        drop(claim);

        assert_eq!(
            super::super::onboarding::begin_attach(&path)
                .expect("return remains retryable")
                .moment(),
            super::super::onboarding::AttachMoment::Return
        );
    }

    #[test]
    fn delivered_return_notice_commits_onboarding_claim() {
        use phux_config::widget::WidgetRegistry;
        use phux_config::{StatusCfg, Widget};

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("onboarding.json");
        let claim = returning_onboarding_claim(&path);
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..StatusCfg::default()
        };
        let bar = phux_config::widget::StatusBar::build(&cfg, &WidgetRegistry::with_builtins())
            .expect("bar");
        let mut painter =
            StatusBarPainter::new(bar, crate::render::chrome::status_bar::Position::Bottom);
        assert!(apply_initial_notice(
            Some(&mut painter),
            Some(Notice::info(super::super::onboarding::RETURN_NOTICE)),
        ));

        let mut out = Vec::new();
        let delivered = paint_bar_after_pane(
            Some(&mut painter),
            &mut out,
            (80, 24),
            None,
            "demo",
            None,
            None,
            false,
        );
        assert!(matches!(delivered, StatusBarPaint::Published { .. }));
        let mut escaped = false;
        let plain: String = String::from_utf8_lossy(&out)
            .chars()
            .filter(|ch| {
                if escaped {
                    if ch.is_ascii_alphabetic() {
                        escaped = false;
                    }
                    false
                } else if *ch == '\x1b' {
                    escaped = true;
                    false
                } else {
                    true
                }
            })
            .collect();
        assert!(
            plain.contains(super::super::onboarding::RETURN_NOTICE),
            "painted text: {plain:?}"
        );
        let mut claim = Some(claim);
        finish_return_onboarding_after_paint(&mut claim, Some(&painter), delivered);

        assert!(super::super::onboarding::begin_attach(&path).is_none());
    }

    #[test]
    fn truncated_return_notice_retries_until_the_full_notice_is_published() {
        use phux_config::widget::WidgetRegistry;
        use phux_config::{StatusCfg, Widget};

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("onboarding.json");
        let mut claim = Some(returning_onboarding_claim(&path));
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..StatusCfg::default()
        };
        let bar = phux_config::widget::StatusBar::build(&cfg, &WidgetRegistry::with_builtins())
            .expect("bar");
        let mut painter =
            StatusBarPainter::new(bar, crate::render::chrome::status_bar::Position::Bottom);
        assert!(apply_initial_notice(
            Some(&mut painter),
            Some(Notice::info(super::super::onboarding::RETURN_NOTICE)),
        ));

        let mut out = Vec::new();
        let truncated = paint_bar_after_pane(
            Some(&mut painter),
            &mut out,
            (20, 24),
            None,
            "demo",
            None,
            None,
            false,
        );
        finish_return_onboarding_after_paint(&mut claim, Some(&painter), truncated);
        assert!(claim.is_some(), "a truncated notice must remain retryable");

        let delivered = paint_bar_after_pane(
            Some(&mut painter),
            &mut out,
            (80, 24),
            None,
            "demo",
            None,
            None,
            false,
        );
        finish_return_onboarding_after_paint(&mut claim, Some(&painter), delivered);

        assert!(claim.is_none());
        assert!(super::super::onboarding::begin_attach(&path).is_none());
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

    // -- lenient resolver at attach (phux-i0e8.3.4) -----------------------

    #[test]
    fn attach_resolver_survives_one_bad_chord_and_keeps_detach() {
        // Before phux-i0e8.3.4, one malformed chord ("q-") made
        // build_resolver_from return None: EVERY binding died, including
        // detach. Now the attach path always gets a resolver and only the
        // offending binding is disabled.
        let cfg = phux_config::parse_str(
            r#"
            [keybindings.prefix-table]
            "q-" = "kill-pane"
            d = "detach"
            "#,
            Path::new("test.toml"),
        )
        .expect("test config parses");
        let (mut resolver, diags) = build_resolver_from(&cfg.keybindings);
        assert_eq!(diags.len(), 1, "exactly the bad binding is reported");
        assert_eq!(diags[0].binding, "q-");

        let prefix = phux_config::keybind::parse_chord(&cfg.keybindings.prefix).expect("prefix");
        assert_eq!(resolver.feed(prefix), phux_config::keybind::Feed::Partial);
        match resolver.feed(phux_config::keybind::parse_chord("d").expect("chord")) {
            phux_config::keybind::Feed::Resolved(ra) => assert_eq!(ra.action, "detach"),
            other => panic!("detach must survive one bad chord, got {other:?}"),
        }
    }

    #[test]
    fn keybind_error_line_names_the_chord_and_config_check() {
        let cfg = phux_config::parse_str(
            r#"
            [keybindings.prefix-table]
            "q-" = "kill-pane"
            "#,
            Path::new("test.toml"),
        )
        .expect("test config parses");
        let (_, diags) = build_resolver_from(&cfg.keybindings);
        let line = keybind_error_line(&diags);
        assert!(
            line.contains("\"q-\""),
            "line must name the offending chord: {line}"
        );
        assert!(
            line.contains("run: phux config check"),
            "line must point at the checker: {line}"
        );
        assert!(
            !line.contains("more;"),
            "a single diagnostic carries no +N count: {line}"
        );
    }

    #[test]
    fn keybind_error_line_counts_additional_disabled_bindings() {
        let cfg = phux_config::parse_str(
            r#"
            [keybindings.prefix-table]
            "q-" = "kill-pane"
            "w-" = "kill-pane"
            "e-" = "kill-pane"
            "#,
            Path::new("test.toml"),
        )
        .expect("test config parses");
        let (_, diags) = build_resolver_from(&cfg.keybindings);
        assert_eq!(diags.len(), 3);
        let line = keybind_error_line(&diags);
        // BTreeMap order: "e-" first, the other two summarized.
        assert!(
            line.contains("\"e-\""),
            "line must name the first offending chord: {line}"
        );
        assert!(
            line.contains("+2 more; run: phux config check"),
            "line must count the remaining disabled bindings: {line}"
        );
    }

    #[test]
    fn keybind_error_line_is_empty_without_diagnostics() {
        assert_eq!(keybind_error_line(&[]), "");
    }

    #[test]
    fn config_error_line_recommends_config_check() {
        // phux-i0e8.3.5: the remedy is the verb that diagnoses
        // (`config check`), not the one that merely renders the
        // effective config (`config show`).
        let line = config_error_line(&"boom");
        assert!(
            line.contains("phux config check"),
            "line must point at the checker: {line}"
        );
        assert!(
            !line.contains("config show"),
            "line must not recommend config show: {line}"
        );
        assert!(
            line.contains("config error: boom"),
            "line must carry the error display: {line}"
        );
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
        overlays.push(palette_overlay());
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
    fn output_honors_coalescing_decision() {
        let output = FrameKind::TerminalOutput {
            terminal_id: TerminalId::Local { id: 1 },
            stream_id: phux_protocol::StreamId::new(1).expect("stream"),
            bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
            seq: 1,
            bytes: bytes::Bytes::new(),
        };
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
        let (_, _, panes) = published_test_state(&[(&id, 80, 24, b"\x1b]2;~/src/phux\x07")]);

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
        let (_, _, panes) = published_test_state(&[(&id, 80, 24, b"\x1b]2;claude task\x07")]);

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
        let (_, _, mut panes) = published_test_state(&[
            (&claude, 80, 24, b"\x1b]2;Claude Code - ~/src/phux\x07"),
            (&shell, 80, 24, b"\x1b]2;~/src/phux\x07"),
        ]);

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
        let ack = Some((
            TerminalId::local(7),
            phux_protocol::StreamId::new(1).expect("stream"),
            phux_protocol::BootstrapId::new(1).expect("bootstrap"),
            42u64,
        ));
        assert_eq!(should_emit_frame_ack(false, ack), None);
    }

    #[test]
    fn state_sync_consumer_emits_frame_ack() {
        let ack = Some((
            TerminalId::local(7),
            phux_protocol::StreamId::new(1).expect("stream"),
            phux_protocol::BootstrapId::new(1).expect("bootstrap"),
            42u64,
        ));
        assert_eq!(should_emit_frame_ack(true, ack.clone()), ack);
        assert_eq!(should_emit_frame_ack(true, None), None);
    }

    #[test]
    fn terminal_replies_require_negotiated_server_feature() {
        let reply = (TerminalId::local(7), b"\x1b[0n".to_vec());
        let mut supported = FrameOutcome {
            pty_writes: vec![reply.clone()],
            ..FrameOutcome::default()
        };
        assert_eq!(
            take_terminal_replies(&mut supported, true),
            vec![reply.clone()]
        );
        assert!(supported.notices.is_empty());

        let mut old_server = FrameOutcome {
            pty_writes: vec![reply],
            ..FrameOutcome::default()
        };
        assert!(take_terminal_replies(&mut old_server, false).is_empty());
        assert!(old_server.pty_writes.is_empty());
        assert_eq!(old_server.notices.len(), 1);
        assert!(old_server.notices[0].text.contains("terminal-reply"));
    }

    /// phux-501l hardening: an outcome that ends the loop writes nothing.
    ///
    /// Both call sites send terminal replies before they read `outcome.exit`,
    /// so an outcome carrying both would write into a session it is already
    /// abandoning. Suppression belongs here, at the seam the two share, rather
    /// than at either one.
    ///
    /// Note the `terminal_reply_supported = true` argument: this must hold on
    /// the path where replies are otherwise perfectly sendable. It is the exit,
    /// not the feature negotiation, that makes them pointless.
    #[test]
    fn an_exiting_outcome_sends_no_terminal_reply() {
        let mut exiting = FrameOutcome {
            pty_writes: vec![(TerminalId::local(7), b"\x1b[0n".to_vec())],
            exit: true,
            exit_reason: Some(AttachEnd::LastPaneClosed {
                exit_status: Some(7),
            }),
            ..FrameOutcome::default()
        };
        assert!(
            take_terminal_replies(&mut exiting, true).is_empty(),
            "an ended session has no PTY to answer; writing here races the server's own exit",
        );
        assert!(exiting.pty_writes.is_empty());
        // No notice: this is the normal end of a session, not a degradation
        // the user needs told about. The `LastPaneClosed` explanation is what
        // the CLI prints, and it must survive intact.
        assert!(exiting.notices.is_empty());
        assert_eq!(
            exiting.exit_reason,
            Some(AttachEnd::LastPaneClosed {
                exit_status: Some(7)
            })
        );
    }

    /// phux-501l, the actual defect: a write that fails because the peer is
    /// already gone must not become the reason the attach loop ended.
    ///
    /// The last pane's shell exits, so the server emits `TERMINAL_OUTPUT` then
    /// `TERMINAL_CLOSED` back to back and exits, closing the socket. One client
    /// read pulls both frames. Acking the output writes into the dead socket
    /// and, before this, killed the loop with `Io(BrokenPipe)` — so the
    /// `TERMINAL_CLOSED` sitting in the *same batch* was never processed and
    /// "the last pane exited 7" was replaced by "attach loop io error".
    ///
    /// The classifier is what lets the loop keep going and end for the reason
    /// the frames give. A genuine local IO fault must still be fatal, so the
    /// discrimination is on `ErrorKind`, not on "any Io".
    #[test]
    fn a_write_to_a_departed_peer_is_not_a_loop_ending_error() {
        for kind in [
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::ConnectionAborted,
        ] {
            assert!(
                peer_gone(&AttachError::Io(io::Error::from(kind))),
                "{kind:?} means the peer hung up, not that this process faulted",
            );
        }

        // A real local failure is still fatal: if the server were still there,
        // the write would not have failed, so these cannot be a departed peer.
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::OutOfMemory,
            io::ErrorKind::InvalidData,
        ] {
            assert!(
                !peer_gone(&AttachError::Io(io::Error::from(kind))),
                "{kind:?} is a local fault and must still fail the loop",
            );
        }

        // Non-Io endings are classified by their own variants and must never
        // be swallowed as a departed peer.
        assert!(!peer_gone(&AttachError::Disconnected));
        assert!(!peer_gone(&AttachError::Protocol("bad frame".to_owned())));
    }

    #[test]
    fn headless_completion_drains_history_and_metadata_after_attach_ready() {
        let terminal_id = TerminalId::local(7);
        let stream_id = phux_protocol::StreamId::new(1).expect("stream");
        let bootstrap_id = phux_protocol::BootstrapId::new(1).expect("bootstrap");
        let mut completion = HeadlessCompletion::new(Some(1));
        completion.note_history_request(&terminal_id, stream_id, bootstrap_id);

        completion.observe_frame(&FrameKind::AttachReady { attach_id: 7 }, 7);
        assert!(
            !completion.is_complete(false),
            "ATTACH_READY may be queued before history and metadata replies"
        );
        completion.observe_frame(
            &FrameKind::MetadataValue {
                request_id: 1,
                value: None,
            },
            7,
        );
        assert!(
            !completion.is_complete(true),
            "layout and agent replies do not complete a pending history chain"
        );
        completion.observe_frame(
            &FrameKind::HistoryPage {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                rows: 0,
                page_seq: 1,
                cursor: bytes::Bytes::from_static(b"newest"),
                next_cursor: Some(bytes::Bytes::from_static(b"older")),
                payload: bytes::Bytes::new(),
            },
            7,
        );
        completion.note_history_request(&terminal_id, stream_id, bootstrap_id);
        assert!(
            !completion.is_complete(true),
            "an intermediate history page keeps its generation pending"
        );
        completion.observe_frame(
            &FrameKind::HistoryPage {
                terminal_id,
                stream_id,
                rows: 0,
                bootstrap_id,
                page_seq: 1,
                cursor: bytes::Bytes::from_static(b"older"),
                next_cursor: None,
                payload: bytes::Bytes::new(),
            },
            7,
        );
        assert!(completion.is_complete(true));
    }

    #[test]
    fn headless_history_control_responses_clear_outstanding_request() {
        let terminal_id = TerminalId::local(8);
        let stream_id = phux_protocol::StreamId::new(1).expect("stream");
        let bootstrap_id = phux_protocol::BootstrapId::new(1).expect("bootstrap");
        let terminal_frames = [
            FrameKind::HistoryTombstone {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                cursor: bytes::Bytes::from_static(b"cursor"),
                reason: phux_protocol::wire::frame::HistoryTombstoneReason::Pruned,
            },
            FrameKind::HistoryRejected {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                cursor: bytes::Bytes::from_static(b"cursor"),
                reason: phux_protocol::wire::frame::HistoryRejectionReason::TooSmall,
                required_bytes: 128,
                required_rows: 1,
            },
        ];
        for frame in terminal_frames {
            let mut completion = HeadlessCompletion::new(None);
            completion.observe_frame(&FrameKind::AttachReady { attach_id: 7 }, 7);
            completion.note_history_request(&terminal_id, stream_id, bootstrap_id);
            completion.observe_frame(&frame, 7);
            assert!(completion.is_complete(true));
        }
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
    async fn attach_negotiation_waits_for_hello_ok_and_sends_one_hello() {
        let (client_stream, server_stream) = UnixStream::pair().expect("pair");
        let mut client = Connection::from_stream(client_stream);
        let server =
            tokio::spawn(ScriptedServer::on_stream(server_stream, ScriptSpec::new()).run());

        let res = client
            .negotiate(attach_client_name(), attach_client_caps(None))
            .await;
        assert!(
            res.is_ok(),
            "handshake should succeed when HELLO_OK arrives"
        );
        let selected = client
            .negotiated_bootstrap()
            .expect("successful negotiation installs immutable profile state");
        assert_eq!(selected.limits, phux_protocol::BootstrapLimits::default());
        let duplicate = client
            .negotiate(attach_client_name(), attach_client_caps(None))
            .await;
        assert!(
            matches!(duplicate, Err(AttachError::Protocol(_))),
            "a second local negotiation must be rejected before it reaches the wire"
        );
        drop(client);
        let seen = server.await.expect("scripted server task");
        assert!(
            matches!(seen.as_slice(), [FrameKind::Hello { .. }]),
            "attach construction must send exactly one HELLO, got {seen:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn attach_negotiation_preserves_custom_caps_then_sends_attach() {
        let colors = TerminalDefaultColors {
            foreground: TerminalColor { r: 1, g: 2, b: 3 },
            background: TerminalColor { r: 4, g: 5, b: 6 },
        };
        let (client_stream, server_stream) = UnixStream::pair().expect("pair");
        let mut client = Connection::from_stream(client_stream);
        let mut server = Connection::from_stream(server_stream);

        let client_side = async {
            client
                .negotiate(attach_client_name(), attach_client_caps(Some(colors)))
                .await
                .expect("HELLO_OK");
            client
                .send(&FrameKind::Attach {
                    attach_id: 1,
                    target: AttachTarget::Last,
                    viewport: ViewportInfo::new(120, 40),
                    request_scrollback: true,
                    scrollback_limit_lines: 10_000,
                })
                .await
                .expect("ATTACH");
        };
        let server_side = async {
            let hello = server.recv().await.expect("HELLO");
            let FrameKind::Hello { client_caps, .. } = &hello else {
                panic!("expected HELLO");
            };
            let (selected_profile, bootstrap_limits) =
                select_bootstrap_profile(client_caps, &BootstrapCapabilities::new())
                    .expect("fixture profiles intersect");
            server
                .send(&FrameKind::HelloOk {
                    protocol_major: PROTOCOL_VERSION.major,
                    protocol_minor: PROTOCOL_VERSION.minor,
                    protocol_patch: PROTOCOL_VERSION.patch,
                    server_caps: ServerCapabilities::new(),
                    server_id: Vec::new(),
                    selected_profile,
                    bootstrap_limits,
                })
                .await
                .expect("HELLO_OK");
            let attach = server.recv().await.expect("ATTACH");
            (hello, attach)
        };

        let ((), (hello, attach)) = tokio::join!(client_side, server_side);
        let FrameKind::Hello { client_caps, .. } = hello else {
            panic!("first frame must be HELLO");
        };
        assert_eq!(client_caps.default_colors, Some(colors));
        assert!(client_caps.layers.contains(Layer::L3));
        assert!(
            matches!(attach, FrameKind::Attach { .. }),
            "ATTACH must immediately follow the single HELLO exchange"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn attach_negotiation_rejects_non_hello_ok_reply() {
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
                .send(&FrameKind::Detached {
                    reason: Some(DetachReason::ProtocolError),
                    message: String::new(),
                })
                .await
                .expect("server send detached");
        };

        let negotiation = client.negotiate(attach_client_name(), attach_client_caps(None));
        let (res, ()) = tokio::join!(negotiation, server_side);
        match res {
            Err(AttachError::Protocol(msg)) => {
                // phux-i0e8.7.3: a frame with no arm is explained as version
                // skew with a remedy, never dumped as a Debug rendering.
                assert!(msg.contains("unexpected HELLO reply"), "{msg}");
                assert!(msg.contains("run `phux doctor`"), "{msg}");
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

    #[test]
    fn clean_detach_records_the_complete_reset_before_finalizing() {
        let _guard = TERMINAL_RESET_TEST_LOCK
            .lock()
            .expect("terminal reset test lock");
        HOVER_TRACKING_ACTIVE.store(true, Ordering::SeqCst);
        MOUSE_CAPTURE_ACTIVE.store(true, Ordering::SeqCst);
        ALT_SCREEN_ACTIVE.store(true, Ordering::SeqCst);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("detach.cast");
        let recorder = Rc::new(RefCell::new(
            SessionRecorder::create(&path, None, phux_record::cast::CastVersion::V2)
                .expect("recorder"),
        ));
        let mut reset = Vec::new();
        write_terminal_reset_and_finalize(&mut reset, Some(&recorder));

        let cast = std::fs::read(&path).expect("read cast");
        let text = String::from_utf8(cast.clone()).expect("cast utf-8");
        assert!(
            text.lines()
                .next()
                .is_some_and(|line| line.contains("\"duration\":")),
            "clean detach must backfill duration"
        );
        let (_, events) = phux_record::cast::read_cast(cast.as_slice()).expect("parse cast");
        let recorded_reset: String = events
            .iter()
            .filter(|event| event.code == phux_record::cast::EventCode::Output)
            .map(|event| event.data.as_str())
            .collect();
        assert_eq!(
            recorded_reset.as_bytes(),
            reset,
            "the recorder must capture every reset byte before it is finalized"
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

    /// A composited frame — panes, dividers and the **shipped** status bar
    /// — at an arbitrary viewport, replayed through the PTY-probe oracle.
    ///
    /// This is the closest the repo gets to "what is on the user's glass":
    /// it runs the real `paint_full_frame` path with the bar built from
    /// `default.toml`, so the shipped `[status]` lineup, the responsive
    /// slot policy, and the widget-level shrink ladders are all exercised
    /// together rather than one layer at a time.
    fn shipped_frame_rows(view: (u16, u16), windows: &[WindowInfo]) -> Vec<String> {
        let (cols, rows) = view;
        let id = TerminalId::local(1);
        let workspace = Workspace::single(id.clone());

        let cfg = phux_config::parse_with_defaults("", std::path::Path::new("/nonexistent/c.toml"))
            .expect("shipped defaults parse");
        let mut status_bar = crate::attach::reload::compose_status_bar(&cfg, &[])
            .expect("the shipped status lineup must build")
            .expect("the shipped lineup is non-empty");
        status_bar.set_windows(windows.to_vec());

        // One row of the viewport belongs to the bar.
        let pane_rows = rows.saturating_sub(1);
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        panes.insert(
            id.clone(),
            PaneSlot::new_with_size(cols, pane_rows).expect("pane slot"),
        );
        let engine_kernel = published_test_kernel(&id, cols, pane_rows, PROBE_PANE_TEXT.as_bytes());

        let mut out: Vec<u8> = Vec::new();
        paint_full_frame(
            &mut out,
            &workspace.render_window(None).expect("layout"),
            &mut panes,
            &engine_kernel,
            Some(&id),
            view,
            Some(&mut status_bar),
            None,
            None,
            "phux",
        );

        let mut probe = PaneSlot::new_with_size(cols, rows).expect("probe slot");
        probe.terminal.vt_write(&out);
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
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    fn probe_window(name: &str, active: bool) -> WindowInfo {
        WindowInfo {
            name: name.to_owned(),
            active,
            zoomed: false,
            attention: false,
            branch: None,
        }
    }

    /// The whole composited frame at a roomy viewport: padded tab strip,
    /// hints, session name and clock, all on one bar row.
    #[test]
    fn shipped_frame_at_a_roomy_viewport() {
        let windows = [
            probe_window("zsh", false),
            probe_window("nvim", true),
            probe_window("server", false),
        ];
        let rows = shipped_frame_rows((100, 12), &windows);
        let bar = rows.last().expect("a bar row");
        assert!(bar.contains(" 1:nvim "), "{bar:?}");
        assert!(bar.contains("Space palette"), "{bar:?}");
        assert!(bar.contains("phux"), "{bar:?}");
        assert!(!bar.contains("switch"), "{bar:?}");
        assert!(rows.join("\n").contains(PROBE_PANE_TEXT), "{rows:?}");
    }

    /// The same frame on a phone-sized grid. This is the shape the
    /// responsive work exists for: the hints and the clock are gone and a
    /// `switch` chip has taken their place, while every tab that is shown
    /// is shown whole.
    #[test]
    fn shipped_frame_at_a_phone_sized_viewport() {
        let windows = [
            probe_window("zsh", false),
            probe_window("nvim", true),
            probe_window("server", false),
            probe_window("logs", false),
        ];
        let rows = shipped_frame_rows((46, 12), &windows);
        let bar = rows.last().expect("a bar row");
        assert!(bar.contains(" 1:nvim "), "active tab whole: {bar:?}");
        assert!(bar.contains("switch"), "{bar:?}");
        assert!(!bar.contains("Space palette"), "hints yield: {bar:?}");
        assert!(bar.chars().count() <= 46, "row overran: {bar:?}");
        assert!(rows.join("\n").contains(PROBE_PANE_TEXT), "{rows:?}");
        insta::assert_snapshot!("shipped_frame_phone_sized", rows.join("\n"));
    }

    /// Narrower still, where the tab strip itself has to give: it keeps
    /// the active tab and its neighbours whole and stands in for the rest
    /// with a `›`, rather than clipping a label into a window name that
    /// does not exist.
    #[test]
    fn shipped_frame_when_the_tab_strip_must_collapse() {
        let windows = [
            probe_window("zsh", false),
            probe_window("nvim", true),
            probe_window("server", false),
            probe_window("logs", false),
        ];
        let rows = shipped_frame_rows((36, 10), &windows);
        let bar = rows.last().expect("a bar row");
        assert!(bar.contains(" 1:nvim "), "active tab whole: {bar:?}");
        assert!(bar.contains('\u{203a}'), "hidden tabs are marked: {bar:?}");
        assert!(!bar.contains("3:logs"), "the far tab is dropped: {bar:?}");
        assert!(bar.contains("switch"), "affordance survives: {bar:?}");
        assert!(bar.chars().count() <= 36, "row overran: {bar:?}");
        insta::assert_snapshot!("shipped_frame_collapsed_tabs", rows.join("\n"));
    }

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
        // Pane renderer metadata is separate from the published engine replica.
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        panes.insert(
            id.clone(),
            PaneSlot::new_with_size(PROBE_VIEW.0 - PROBE_SIDEBAR_W, PROBE_VIEW.1)
                .expect("pane slot"),
        );
        let engine_kernel = published_test_kernel(
            &id,
            PROBE_VIEW.0 - PROBE_SIDEBAR_W,
            PROBE_VIEW.1,
            PROBE_PANE_TEXT.as_bytes(),
        );

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
            &engine_kernel,
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
    /// base-frame path, so which-key, prompts, pickers, and toasts
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
