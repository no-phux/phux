//! Resolved-action side effects: `ActionEffects`, the chord consumer,
//! the effect applier, and the `ReattachTarget` vocabulary.

//! Input dispatcher: translates parser-emitted events into wire frames
//! or layout-action effects.
//!
//! Owns the resolver-intercept path (prefix chord → `ResolvedAction` →
//! mutate the active window of the `Workspace`), the predict overlay's
//! keystroke feed, and the parked-spawn bookkeeping (`PendingSplit` /
//! `PendingWindow`) that bridges a local `split-pane` / `new-window`
//! chord to its remote `SPAWN_RESOURCE` reply.

use std::collections::{HashMap, HashSet};

use phux_protocol::ResourceId;
use phux_protocol::wire::frame::{FrameKind, SESSION_NAME_KEY, Scope};

use crate::attach::actions::{self, PendingSplit, PendingWindow};
use crate::attach::connection::Connection;
use crate::attach::directory_picker::PendingDirectory;
use crate::attach::focus::FocusHistory;
use crate::attach::outcome::AttachError;
use crate::attach::pane_state::{PaneSlot, reanchor_predict_to_pane};
use crate::attach::plugin_actions::PluginRunResult;
use crate::layout::{SplitDir, Workspace};
use crate::predict::PredictionState;
use phux_client::layout_ops::{DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, layout_key};

use super::ctx::DispatchCtx;
use super::dispatch::apply_focus_transition;

/// Apply the side-effects of a resolved action: layout-mutation repaint
/// signal, focus move, prediction reset, `SET_METADATA` broadcast, bell,
/// detach, parked spawn (split / new-window), and kill-frame sequences.
///
/// Shared by the keybinding path and the overlay-commit path (phux-ahv.1)
/// so a rename committed from the prompt broadcasts and repaints exactly
/// like a keybinding would. Returns `true` if the layout changed (the
/// caller repaints).
///
/// The body is a flat, ordered sequence of per-effect appliers. The order
/// effects are applied in is observable in the rendered result, so that
/// order is the one thing this function encodes on its own.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
#[allow(
    clippy::too_many_arguments,
    reason = "shares the dispatch loop's transport + render + predict context; phux-7ry0 added the focused-pane map for the predict re-anchor"
)]
pub(super) async fn apply_action_effects<W: crate::attach::RenderSink>(
    effects: ActionEffects,
    out: &mut W,
    conn: &mut Connection,
    ctx: &mut DispatchCtx<'_>,
    focused_resource: &mut Option<ResourceId>,
    detach_pending: &mut bool,
    predict: &mut PredictionState,
    panes: &HashMap<ResourceId, PaneSlot>,
) -> Result<bool, AttachError> {
    let mut layout_changed = effects.layout_mutated;
    apply_zoom_toggle(effects.toggle_zoom, ctx.zoomed, focused_resource.as_ref());
    apply_sidebar_toggle(effects.toggle_sidebar, ctx.sidebar_enabled);
    apply_focus_effect(
        effects.set_focus,
        effects.clear_predict,
        &mut ctx.focus_history,
        focused_resource,
        predict,
        panes,
    );
    send_layout_metadata(
        effects.set_metadata,
        ctx.layout_read_complete,
        conn,
        ctx.workspace,
        ctx.focused_session,
        ctx.next_request_id,
    )
    .await?;
    if effects.bell {
        let _ = actions::write_bell(out);
    }
    send_detach(effects.detach, conn, detach_pending).await?;
    send_parked_spawns(
        effects.spawn_terminal,
        effects.spawn_window,
        conn,
        ctx.pending_splits,
        ctx.pending_windows,
    )
    .await?;
    send_directory_request(effects.list_directory, conn, ctx.pending_directory).await?;
    send_kill_frames(
        effects.kill_frames,
        effects.expected_closes,
        conn,
        ctx.expected_closes,
    )
    .await?;
    send_command_frames(effects.command_frames, conn).await?;
    spawn_plugin_run(effects.run_plugin, ctx.plugin_tx);
    layout_changed |= Box::pin(apply_pane_move(
        effects.move_pane,
        ctx,
        focused_resource,
        predict,
        panes,
    ))
    .await;
    // phux-foz.5: hand a `reload-config` up to the driver, which owns the
    // config-derived state (resolver, theme, keybindings snapshot, status
    // bar) this batch is still borrowing.
    if effects.reload_config {
        *ctx.reload_request = true;
    }
    record_reattach_request(effects.reattach, ctx.switch_request, ctx.session_name);
    let renamed = send_session_rename(
        effects.rename_session,
        conn,
        ctx.session_name,
        ctx.next_request_id,
    )
    .await?;
    // A rename repaints the status bar (it carries the session name) the
    // same way a layout mutation does; fold it into the caller's repaint
    // signal so the new name shows immediately.
    Ok(layout_changed || renamed)
}

/// Execute a picker-committed pane move on a dedicated control connection.
///
/// The live attach connection must remain owned by the full-duplex driver: a
/// correlated metadata helper would otherwise consume pane output or events
/// interleaved before its reply. A fresh connection also lets operation
/// failures remain a visible toast rather than tearing down the attach.
#[allow(
    clippy::future_not_send,
    reason = "ADR-0003 binds TUI state and its async dispatcher to the current thread"
)]
async fn apply_pane_move(
    intent: Option<PaneMoveIntent>,
    ctx: &mut DispatchCtx<'_>,
    focused_resource: &mut Option<ResourceId>,
    predict: &mut PredictionState,
    panes: &HashMap<ResourceId, PaneSlot>,
) -> bool {
    let Some(intent) = intent else {
        return false;
    };
    let result = async {
        let dial = ctx.control_dial.ok_or_else(|| {
            AttachError::Protocol("pane move has no dedicated control dial".to_owned())
        })?;
        let mut conn = Connection::connect_dial(dial).await?;
        phux_client::pane_move::move_pane(
            &mut conn,
            intent.source.clone(),
            intent.target,
            intent.dir,
            intent.ratio,
        )
        .await
        .map_err(|error| AttachError::Protocol(error.to_string()))
    }
    .await;

    match result {
        Ok(outcome) if outcome.cross_session => {
            let Some((window, pane)) =
                pane_coordinates(&outcome.destination_workspace, &intent.source)
            else {
                push_move_failure(
                    ctx,
                    "the move committed, but the destination layout no longer contains the pane",
                );
                return false;
            };
            *ctx.switch_request = Some(ReattachTarget::Existing {
                name: outcome.destination_session_name,
                window: Some(window),
                pane: Some(pane),
            });
            true
        }
        Ok(outcome) => {
            *ctx.workspace = outcome.destination_workspace;
            *ctx.zoomed = None;
            apply_focus_effect(
                Some(intent.source),
                true,
                &mut ctx.focus_history,
                focused_resource,
                predict,
                panes,
            );
            true
        }
        Err(error) => {
            tracing::warn!(error = %error, "move-pane failed");
            push_move_failure(ctx, &error.to_string());
            false
        }
    }
}

fn pane_coordinates(workspace: &Workspace, target: &ResourceId) -> Option<(usize, usize)> {
    workspace
        .windows
        .iter()
        .enumerate()
        .find_map(|(window, item)| {
            let tree = item.state.tree.as_ref()?;
            crate::layout::leaves(tree)
                .iter()
                .position(|pane| pane == target)
                .map(|pane| (window, pane))
        })
}

fn push_move_failure(ctx: &mut DispatchCtx<'_>, message: &str) {
    ctx.overlays
        .push(Box::new(crate::render::overlay::ToastOverlay::new(
            "Pane move failed",
            vec![message.to_owned()],
            ctx.theme,
        )));
}

/// phux-x2hm: flip pane-zoom. Un-zoom if zoomed; otherwise zoom the
/// focused pane. `run_action` already gated single-pane windows.
fn apply_zoom_toggle(
    toggle: bool,
    zoomed: &mut Option<ResourceId>,
    focused_resource: Option<&ResourceId>,
) {
    if !toggle {
        return;
    }
    *zoomed = if zoomed.is_some() {
        None
    } else {
        focused_resource.cloned()
    };
}

/// phux-4h5a: flip the window-sidebar on/off state. The driver re-folds
/// `sidebar_enabled` into the per-frame reservation after dispatch, so
/// the `layout_mutated` repaint tiles into the new content rect.
const fn apply_sidebar_toggle(toggle: bool, sidebar_enabled: &mut bool) {
    if toggle {
        *sidebar_enabled = !*sidebar_enabled;
    }
}

/// Move the driver's focused pane, or — when the action only invalidated
/// the prediction queue — drop that queue.
///
/// Focus moved (keybinding pane navigation) — re-anchor predict to
/// the new pane: reset its cursor + viewport and drop the old pane's
/// queue, so a keystroke before the next reconcile echoes at the
/// right place rather than the old pane's (mid-screen) coordinates
/// (phux-7ry0). Subsumes the plain `clear_predict` drop.
fn apply_focus_effect(
    set_focus: Option<ResourceId>,
    clear_predict: bool,
    focus_history: &mut FocusHistory,
    focused_resource: &mut Option<ResourceId>,
    predict: &mut PredictionState,
    panes: &HashMap<ResourceId, PaneSlot>,
) {
    let Some(target) = set_focus else {
        if clear_predict {
            predict.clear();
        }
        return;
    };
    apply_focus_transition(focus_history, focused_resource, target);
    if let Some(fid) = focused_resource.as_ref() {
        reanchor_predict_to_pane(predict, panes, fid);
    }
}

/// Broadcast the mutated layout envelope as a `SET_METADATA`.
///
/// Encoding can fail only on an empty workspace (we just produced
/// it — shouldn't happen), but propagate cleanly if it ever does.
/// phux-jy4t: keyed per session so a split here persists to THIS
/// session's layout, not a key every session shares.
async fn send_layout_metadata(
    set_metadata: bool,
    layout_read_complete: bool,
    conn: &mut Connection,
    workspace: &Workspace,
    focused_session: Option<phux_protocol::ids::SessionId>,
    next_request_id: &mut u32,
) -> Result<(), AttachError> {
    if !set_metadata || !layout_read_complete {
        return Ok(());
    }
    let Some(session) = focused_session else {
        return Ok(());
    };
    let Some(bytes) = encode_layout_or_log(workspace) else {
        return Ok(());
    };
    let request_id = *next_request_id;
    *next_request_id = next_request_id.wrapping_add(1);
    conn.send(&FrameKind::SetMetadata {
        request_id,
        scope: Scope::Group(DEFAULT_GROUP_ID),
        key: layout_key(session),
        value: bytes,
    })
    .await
}

/// Emit `DETACH` and wait for `DETACHED`; a detach already in flight is
/// not re-sent.
async fn send_detach(
    detach: bool,
    conn: &mut Connection,
    detach_pending: &mut bool,
) -> Result<(), AttachError> {
    if !detach || *detach_pending {
        return Ok(());
    }
    conn.send(&FrameKind::Detach).await?;
    conn.unbind_all_terminals();
    *detach_pending = true;
    Ok(())
}

/// Send the parked `SPAWN_RESOURCE` requests and remember their intent.
///
/// Parked split — send the `SPAWN_RESOURCE` and remember the intent.
/// Parked new-window — same SPAWN flow; the reply opens a window.
async fn send_parked_spawns(
    spawn_terminal: Option<(u32, PendingSplit, FrameKind)>,
    spawn_window: Option<(u32, PendingWindow, FrameKind)>,
    conn: &mut Connection,
    pending_splits: &mut HashMap<u32, PendingSplit>,
    pending_windows: &mut HashMap<u32, PendingWindow>,
) -> Result<(), AttachError> {
    if let Some((request_id, pending, frame)) = spawn_terminal {
        pending_splits.insert(request_id, pending);
        conn.send(&frame).await?;
    }
    if let Some((request_id, pending, frame)) = spawn_window {
        pending_windows.insert(request_id, pending);
        conn.send(&frame).await?;
    }
    Ok(())
}

/// `go-to-directory`: record the listing the picker now waits on, then send
/// the `LIST_DIRECTORY`. Recording first means the reply can never race
/// ahead of the id it has to match.
async fn send_directory_request(
    request: Option<(PendingDirectory, FrameKind)>,
    conn: &mut Connection,
    pending_directory: &mut Option<PendingDirectory>,
) -> Result<(), AttachError> {
    let Some((pending, frame)) = request else {
        return Ok(());
    };
    *pending_directory = Some(pending);
    conn.send(&frame).await
}

/// kill-pane / kill-window keystroke sequences; the `RESOURCE_CLOSED`
/// fold-out happens when each shell exits. Park the targets FIRST
/// (phux-i0e8.2.2): once the frames are on the wire the close can
/// race back, and an unmarked close would notice-spam the user about
/// a death they ordered.
async fn send_kill_frames(
    kill_frames: Vec<FrameKind>,
    targets: Vec<ResourceId>,
    conn: &mut Connection,
    expected_closes: &mut HashSet<ResourceId>,
) -> Result<(), AttachError> {
    expected_closes.extend(targets);
    for frame in kill_frames {
        conn.send(&frame).await?;
    }
    Ok(())
}

/// ADR-0033: supervisory COMMAND frames (take/give the wheel, signal the
/// pane). Fire-and-forward — the server's `COMMAND_RESULT` + `TerminalControl`
/// broadcast drive the chrome; the input loop does not block on the reply.
async fn send_command_frames(
    command_frames: Vec<FrameKind>,
    conn: &mut Connection,
) -> Result<(), AttachError> {
    for frame in command_frames {
        conn.send(&frame).await?;
    }
    Ok(())
}

/// phux-r82.5: a plugin action runs as a spawned child-process task —
/// fire-and-forget from the input loop's perspective. The driver's
/// `select!` picks up the completion report and toasts failures. All
/// client-local (config + exec); nothing goes on the wire (ADR-0017).
fn spawn_plugin_run(
    run_plugin: Option<(String, String)>,
    plugin_tx: Option<&tokio::sync::mpsc::UnboundedSender<PluginRunResult>>,
) {
    let Some((plugin_id, action_id)) = run_plugin else {
        return;
    };
    let Some(tx) = plugin_tx else {
        tracing::warn!(
            plugin = %plugin_id,
            action = %action_id,
            "plugin-action dispatched with no plugin runtime channel; dropping",
        );
        return;
    };
    crate::attach::plugin_actions::spawn_plugin_action(tx.clone(), plugin_id, action_id);
}

/// phux-eb0 / new-session: an in-process re-attach request. Hand the
/// target up to the driver via `ctx.switch_request`; `main_loop` reads
/// it after this dispatch batch and returns a `SwitchTo` exit so the
/// outer loop tears down the current session and re-attaches.
///
/// Switching to the CURRENT session without a window/pane target is a
/// silent no-op. The session picker includes that row for orientation;
/// committing it has already dismissed the overlay, so no reattach is
/// needed. `new-session` is never a no-op — naming an existing session
/// just attaches to it.
fn record_reattach_request(
    reattach: Option<ReattachTarget>,
    switch_request: &mut Option<ReattachTarget>,
    session_name: &str,
) {
    let Some(target) = reattach else {
        return;
    };
    match target {
        ReattachTarget::Existing {
            name,
            window: None,
            pane: None,
        } if name == session_name => {
            tracing::debug!(target_session = %name, "switch-session to current session; no-op");
        }
        ReattachTarget::Existing { name, window, pane } => {
            tracing::info!(target_session = %name, target_window = ?window, target_pane = ?pane, "switch-session requested");
            *switch_request = Some(ReattachTarget::Existing { name, window, pane });
        }
        ReattachTarget::Create(name) => {
            tracing::info!(session = %name, "new-session requested");
            *switch_request = Some(ReattachTarget::Create(name));
        }
    }
}

/// rename-session: since the v0.3.0 "Option B" re-tier (ADR-0019 /
/// ADR-0027) removed the `RENAME_SESSION` verb, a rename is a `SET_METADATA`
/// write of the conventional `SESSION_NAME_KEY` (`Scope::Global`, value
/// `current\0new`); the server intercepts it and applies the registry
/// rename. We optimistically reflect the new name locally — the server is
/// authoritative, and the next ATTACHED snapshot overwrites `session_name`
/// (also how other attached clients learn the rename; a live
/// `SESSION_RENAMED` push is out of scope for this pass). A no-op rename
/// (new == current) is dropped: nothing to send, nothing to repaint.
///
/// Returns `true` when a rename went on the wire — the caller folds that
/// into its repaint signal.
async fn send_session_rename(
    rename_session: Option<String>,
    conn: &mut Connection,
    session_name: &mut String,
    next_request_id: &mut u32,
) -> Result<bool, AttachError> {
    let Some(new_name) = rename_session.filter(|n| n != &*session_name) else {
        return Ok(false);
    };
    let request_id = *next_request_id;
    *next_request_id = next_request_id.wrapping_add(1);
    let mut value = session_name.as_bytes().to_vec();
    value.push(0);
    value.extend_from_slice(new_name.as_bytes());
    conn.send(&FrameKind::SetMetadata {
        request_id,
        scope: Scope::Global,
        key: SESSION_NAME_KEY.to_owned(),
        value,
    })
    .await?;
    tracing::info!(new_name = %new_name, "rename-session sent; optimistically updating local name");
    *session_name = new_name;
    Ok(true)
}

/// Result of feeding a key event through the resolver.
pub(super) enum ChordOutcome {
    /// Chord extended a partial sequence; absorb and wait.
    Partial,
    /// Chord completed a binding; effects follow.
    Resolved(phux_config::keybind::ResolvedAction),
}

/// Convert a `KeyEvent` into a `KeyChord` and feed the resolver. Returns
/// `None` when the resolver is disabled (no config) or the chord
/// doesn't match any binding — caller forwards normally in that case.
///
/// Release / repeat events are NOT fed to the resolver — chord matching
/// is press-only, matching the convention of `phux-config::keybind`'s
/// tests and tmux's prefix table. Repeats of held keys (e.g. arrow keys
/// scrolling) would otherwise re-fire actions per-tick.
pub(super) fn consume_chord(
    ctx: &mut DispatchCtx<'_>,
    key_event: &phux_protocol::input::key::KeyEvent,
) -> Option<ChordOutcome> {
    use phux_protocol::input::key::KeyAction;
    let resolver = ctx.resolver.as_deref_mut()?;
    if !matches!(key_event.action, KeyAction::Press) {
        return None;
    }
    let chord = phux_config::keybind::KeyChord {
        modifiers: key_event.mods,
        key: key_event.key,
    };
    match resolver.feed(chord) {
        phux_config::keybind::Feed::NoMatch => None,
        phux_config::keybind::Feed::Partial => {
            // Mid-chord: the user is partway through a multi-chord binding.
            // Debug (not info) — chord progress is finer-grained than the
            // resolved-action lifecycle event in `run_action`.
            tracing::debug!("chord: partial match, awaiting next chord");
            Some(ChordOutcome::Partial)
        }
        phux_config::keybind::Feed::Resolved(r) => {
            tracing::debug!(action = %r.action, "chord: resolved to action");
            Some(ChordOutcome::Resolved(r))
        }
    }
}

/// Side-effects a resolved action wants from the driver.
#[derive(Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "action dispatcher returns independent side-effect flags to keep async I/O outside run_action"
)]
pub(super) struct ActionEffects {
    /// `true` ⇒ the active window was mutated in-place; driver repaints.
    pub(super) layout_mutated: bool,
    /// phux-x2hm: `true` ⇒ flip the driver's pane-zoom state (zoom the
    /// focused pane to fill the window, or un-zoom). `apply_action_effects`
    /// owns the actual toggle since the `zoomed` state lives in the driver.
    pub(super) toggle_zoom: bool,
    /// phux-4h5a: `true` ⇒ flip the driver's window-sidebar on/off state.
    /// `apply_action_effects` owns the toggle since `sidebar_enabled` lives in
    /// the driver; it also sets `layout_mutated` so the panes reflow into (or
    /// out of) the sidebar's reserved columns on the same-iteration repaint.
    pub(super) toggle_sidebar: bool,
    /// `Some(new_focus)` ⇒ swap the driver's `focused_resource` (input
    /// routing follows). The action helper already updated the active
    /// window's focus; this carries the new id so the driver
    /// doesn't have to re-read it.
    pub(super) set_focus: Option<ResourceId>,
    /// `true` ⇒ emit `SET_METADATA` carrying the new layout envelope.
    pub(super) set_metadata: bool,
    /// `true` ⇒ emit a terminal bell (BEL `\x07`).
    pub(super) bell: bool,
    /// phux-4li.16: `true` ⇒ the active window changed; the driver must
    /// drop the prediction queue (anchored to the old window's focused
    /// pane) so a stale ghost echo doesn't paint into the new window
    /// before the next `RESOURCE_OUTPUT` reconciles.
    pub(super) clear_predict: bool,
    /// `true` ⇒ emit `DETACH` and wait for `DETACHED`.
    pub(super) detach: bool,
    /// phux-4li.12: a `split-pane` action emitted a `SPAWN_RESOURCE`
    /// and parked a [`PendingSplit`] keyed by `request_id`. The async
    /// caller sends the frame, then inserts the parked entry into the
    /// driver-wide `pending_splits` map.
    pub(super) spawn_terminal: Option<(u32, PendingSplit, FrameKind)>,
    /// phux-4li.15: a `new-window` action emitted a `SPAWN_RESOURCE` and
    /// parked a [`PendingWindow`] keyed by `request_id`. The async caller
    /// sends the frame and inserts the parked entry into the driver-wide
    /// `pending_windows` map; the reply opens a new window on the
    /// spawned pane.
    pub(super) spawn_window: Option<(u32, PendingWindow, FrameKind)>,
    /// A picker-confirmed existing-pane move. The async effect applier opens a
    /// dedicated control connection and runs the canonical headless operation.
    pub(super) move_pane: Option<PaneMoveIntent>,
    /// A `go-to-directory` action built a `LIST_DIRECTORY` request. The
    /// async caller records it (id and listed host) as the pending listing,
    /// then sends it; the reply opens the directory picker.
    pub(super) list_directory: Option<(PendingDirectory, FrameKind)>,
    /// phux-4li.12: a `kill-pane` action ships a sequence of frames to
    /// the focused Terminal (the "soft-kill via shell-exit" — see
    /// `run_action`). The async caller sends them in order; the
    /// resulting `RESOURCE_CLOSED` from the server folds the pane out
    /// of the layout in [`crate::attach::server_frame::handle_server_frame`].
    pub(super) kill_frames: Vec<FrameKind>,
    /// phux-i0e8.2.2: the Terminals `kill_frames` targets. The async
    /// caller parks them in `DispatchCtx::expected_closes` so the
    /// eventual `RESOURCE_CLOSED` is recognized as client-initiated and
    /// its pane-exit notice suppressed.
    pub(super) expected_closes: Vec<ResourceId>,
    /// ADR-0033: supervisory commands (`ACQUIRE_INPUT` / `RELEASE_INPUT` /
    /// `SIGNAL_TERMINAL`) the `take-input` / `give-input` / `signal-terminal`
    /// actions built for the focused pane. The async caller sends each as a
    /// `COMMAND` frame in order; the server's `TerminalControl` broadcast (which
    /// we subscribed to at attach) drives the chrome update on the way back.
    pub(super) command_frames: Vec<FrameKind>,
    /// phux-4li.20 / phux-eb0 / new-session: an in-process re-attach the
    /// driver should perform after this batch — either switch to an
    /// existing session or create a new one. [`apply_action_effects`]
    /// hands it up via `DispatchCtx::switch_request`; the driver's
    /// `main_loop` returns a `SwitchTo` exit and the outer loop detaches
    /// and re-attaches on the same connection. An `Existing` request
    /// matching the current session without a window/pane target is a silent
    /// no-op (the session picker uses that row to dismiss in place).
    pub(super) reattach: Option<ReattachTarget>,
    /// rename-session: a committed rename. Carries the new name. The async
    /// caller ([`apply_action_effects`]) sends a `RENAME_SESSION` command
    /// for the *current* session over the existing connection and
    /// optimistically updates the client's own cached `session_name` +
    /// repaints its status bar. The server is authoritative: the next
    /// `ATTACHED` snapshot reconciles the name (and is how other attached
    /// clients learn of it — a live `SESSION_RENAMED` push is out of scope
    /// for this pass). A refusal (unknown session / name taken) arrives as a
    /// `COMMAND_RESULT { Error }`; this pass logs it and lets the next
    /// snapshot correct the optimistic name rather than blocking the input
    /// loop on the reply.
    pub(super) rename_session: Option<String>,
    /// phux-r82.5: a `plugin-action` dispatch carrying
    /// `(plugin_id, action_id)`. The async caller
    /// ([`apply_action_effects`]) spawns the child-process run via
    /// [`crate::attach::plugin_actions::spawn_plugin_action`] so the input loop
    /// never blocks on the plugin; completion lands on the driver's
    /// plugin-events channel (failure output toasts there).
    pub(super) run_plugin: Option<(String, String)>,
    /// phux-foz.5: `true` ⇒ the user asked for a live config reload
    /// (`reload-config`, via palette or a bound chord). Carried up to the
    /// driver via `DispatchCtx::reload_request`; the driver re-runs the
    /// layered loader after this batch and swaps its config-derived
    /// state atomically (old config kept on any failure).
    pub(super) reload_config: bool,
}

/// Exact move selected by the fuzzy destination picker.
#[derive(Debug)]
pub(super) struct PaneMoveIntent {
    pub(super) source: ResourceId,
    pub(super) target: ResourceId,
    pub(super) dir: SplitDir,
    pub(super) ratio: f32,
}

/// An in-process re-attach request raised by a dispatched action.
///
/// Produced by `switch-session` / `new-session` (phux-eb0) and carried up
/// to the driver via `DispatchCtx::switch_request`; `main_loop` returns a
/// `SwitchTo` exit and the outer loop detaches and re-attaches on the same
/// connection without dropping the transport or leaving raw mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReattachTarget {
    /// Switch to an existing session by name (`switch-session`).
    Existing {
        /// Target session name.
        name: String,
        /// phux-foz.8: window index to select once the target session's
        /// persisted layout loads — the one-step cross-session window
        /// pick. `None` keeps the session's own remembered focus. The
        /// index addresses the target's L3 workspace (the same order its
        /// own window picker shows); if the layout changed under us and
        /// the index is out of range, the switch still lands and the
        /// select is a logged no-op.
        window: Option<usize>,
        /// phux-jpqd: DFS leaf ordinal within `window` to focus once the
        /// target's layout loads — the one-step cross-session **pane**
        /// pick the agent-fleet dashboard's foreign rows carry. `None`
        /// keeps the window's own restored focus. Applied only after
        /// `window` resolves in range; an out-of-range ordinal degrades to
        /// a logged no-op, same as `window`.
        pane: Option<usize>,
    },
    /// Create — or attach to, if it already exists — a session by name
    /// (`new-session`).
    Create(String),
}

/// Encode the workspace for `SET_METADATA`, logging encode failures.
/// Returns `None` on failure — caller should not emit a frame in that case.
pub(in crate::attach) fn encode_layout_or_log(workspace: &Workspace) -> Option<Vec<u8>> {
    match workspace.encode_cbor() {
        Ok(bytes) => Some(bytes),
        Err(err) => {
            tracing::warn!(error = %err, "layout CBOR encode failed; SET_METADATA skipped");
            None
        }
    }
}
