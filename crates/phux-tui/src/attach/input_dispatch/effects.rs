//! Resolved-action side effects: `ActionEffects`, the chord consumer,
//! the effect applier, and the `ReattachTarget` vocabulary.

use std::collections::{HashMap, HashSet};

use phux_protocol::ResourceId;
use phux_protocol::ids::SessionId;
use phux_protocol::wire::frame::{FrameKind, Scope};

use crate::attach::actions::{self, PendingSplit, PendingWindow};
use crate::attach::connection::Connection;
use crate::attach::directory_picker::PendingDirectory;
use crate::attach::focus::FocusHistory;
use crate::attach::jobs::{Job, Start};
use crate::attach::outcome::AttachError;
use crate::attach::pane_state::{PaneSlot, reanchor_predict_to_pane};
use crate::layout::{ArrangementDoc, SplitDir};
use crate::predict::PredictionState;
use crate::render::overlay::ToastOverlay;
use phux_client::layout_ops::{DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, layout_key};

use super::ctx::DispatchCtx;

/// Apply a resolved action's side effects, shared by keybindings and overlay
/// commits. The applier order is observable, and is the one thing this
/// function encodes. Returns `true` if the layout changed.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub(super) async fn apply_action_effects<W: crate::attach::RenderSink>(
    effects: ActionEffects,
    out: &mut W,
    conn: &mut Connection,
    ctx: &mut DispatchCtx<'_>,
    focused_resource: &mut Option<ResourceId>,
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
    if effects.set_metadata {
        broadcast_layout(conn, ctx).await?;
    }
    if effects.bell {
        let _ = actions::write_bell(out);
    }
    if let Some(text) = effects.clipboard {
        let _ = out.write_all(&crate::attach::copy::osc52_set_clipboard(&text));
        let _ = out.flush();
    }
    send_detach(effects.detach, conn, ctx.detach_pending).await?;
    send_parked_spawns(
        effects.spawn_terminal,
        effects.spawn_window,
        conn,
        ctx.pending_splits,
        ctx.pending_windows,
    )
    .await?;
    if let Some((request_id, title, frame)) = effects.spawn_floating {
        // Parked before the send: the reply can race back before it returns.
        ctx.pending_floating.insert(request_id, title);
        conn.send(&frame).await?;
    }
    send_directory_request(effects.list_directory, conn, ctx.pending_directory).await?;
    if let Some((pending, frame)) = effects.query_path {
        *ctx.pending_path = Some(pending);
        conn.send(&frame).await?;
    }
    if let Some((target, text)) = effects.insert_path {
        // This is an input event, never a shell command or an Enter key.
        let paste = phux_protocol::input::paste::PasteEvent {
            data: text.into_bytes(),
            trust: phux_protocol::input::paste::PasteTrust::Untrusted,
        };
        conn.send(&phux_protocol::input::InputEvent::Paste(paste).into_frame(target))
            .await?;
    }
    send_kill_frames(
        effects.kill_frames,
        effects.expected_closes,
        effects.kill_requests,
        conn,
        ctx.expected_closes,
        ctx.pending_kills,
    )
    .await?;
    send_command_frames(effects.command_frames, conn).await?;
    layout_changed |= Box::pin(apply_pane_move(
        effects.move_pane,
        ctx,
        focused_resource,
        predict,
        panes,
    ))
    .await;
    super::session_management::apply(effects.session_management, ctx).await;
    start_job(effects.job, ctx);
    // Hand a `reload-config` up to the driver, which owns the
    // config-derived state (resolver, theme, keybindings snapshot, status
    // bar) this batch is still borrowing.
    if effects.reload_config {
        *ctx.reload_request = true;
    }
    if effects.switch_host.is_some() {
        *ctx.host_switch_request = effects.switch_host;
    }
    if effects.toggle_project.is_some() {
        *ctx.project_toggle_request = effects.toggle_project;
    }
    record_reattach_request(
        effects.reattach,
        ctx.switch_request,
        ctx.session_name,
        ctx.peers.focused_session,
    );
    send_session_rename(effects.rename_session, conn, ctx).await?;
    Ok(layout_changed)
}

/// Execute a picker-committed pane move on a dedicated control connection:
/// the attach connection's correlated helpers would consume interleaved pane
/// output, and a failure stays a toast instead of ending the attach.
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
            &[],
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
                id: None,
                window: Some(window),
                pane: Some(pane),
                resource: Some(intent.source),
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

fn pane_coordinates(workspace: &ArrangementDoc, target: &ResourceId) -> Option<(usize, usize)> {
    workspace
        .groups
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

/// Flip pane-zoom. Un-zoom if zoomed; otherwise zoom the
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

/// Flip the window-sidebar on/off state. The driver re-folds
/// `sidebar_enabled` into the per-frame reservation after dispatch, so
/// the `layout_mutated` repaint tiles into the new content rect.
const fn apply_sidebar_toggle(toggle: bool, sidebar_enabled: &mut bool) {
    if toggle {
        *sidebar_enabled = !*sidebar_enabled;
    }
}

/// Move the driver's focus (re-anchoring predict to the new pane), or just
/// drop the prediction queue when only that was asked.
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
    focus_history.transition(focused_resource, Some(target));
    if let Some(fid) = focused_resource.as_ref() {
        reanchor_predict_to_pane(predict, panes, fid);
    }
}

/// Broadcast the workspace on this session's layout key (`SET_METADATA`),
/// once the initial shared read has landed; with an arrangement record,
/// leave the write to the driver after the batch.
#[allow(
    clippy::future_not_send,
    reason = "DispatchCtx holds current-thread state"
)]
pub(super) async fn broadcast_layout(
    conn: &mut Connection,
    ctx: &mut DispatchCtx<'_>,
) -> Result<(), AttachError> {
    if !ctx.layout_read_complete {
        return Ok(());
    }
    if let Some(owed) = ctx.arrangement_write_owed.as_deref_mut() {
        *owed = true;
        return Ok(());
    }
    let Some(session) = ctx.peers.focused_session else {
        return Ok(());
    };
    let Some(bytes) = encode_layout_or_log(ctx.workspace) else {
        return Ok(());
    };
    let request_id = ctx.take_request_id();
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

/// Send the parked split/new-window `SPAWN_RESOURCE`s and remember them.
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

/// Send kill-pane/kill-window `KILL_RESOURCE`s, parking targets and request
/// ids first: the close or refusal can race back before the send returns.
async fn send_kill_frames(
    kill_frames: Vec<FrameKind>,
    targets: Vec<ResourceId>,
    kill_requests: Vec<(u32, ResourceId)>,
    conn: &mut Connection,
    expected_closes: &mut HashSet<ResourceId>,
    pending_kills: &mut HashMap<u32, ResourceId>,
) -> Result<(), AttachError> {
    expected_closes.extend(targets);
    pending_kills.extend(kill_requests);
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

/// Start a background job (ADR-0172): its notice shows as a passthrough
/// toast; a refusal (its kind is exclusive and already running) toasts why.
fn start_job(job: Option<Job>, ctx: &mut DispatchCtx<'_>) {
    let Some(job) = job else {
        return;
    };
    let toast = match ctx.jobs.start(job) {
        Start::Running(None) => return,
        Start::Running(Some(notice)) => {
            ToastOverlay::passthrough(notice.title, notice.lines, ctx.theme)
        }
        Start::Busy(busy) => ToastOverlay::new(busy.title, busy.lines, ctx.theme),
    };
    ctx.overlays.push(Box::new(toast));
}

/// Hand an in-process re-attach to the driver via `ctx.switch_request`.
/// Switching to the current session with no window/pane target is a no-op
/// (the picker's orientation row).
fn record_reattach_request(
    reattach: Option<ReattachTarget>,
    switch_request: &mut Option<ReattachTarget>,
    session_name: &str,
    focused_session: Option<SessionId>,
) {
    let Some(target) = reattach else {
        return;
    };
    match target {
        ReattachTarget::Existing {
            name,
            id,
            window: None,
            pane: None,
            resource: None,
        } if is_current_session(id, &name, session_name, focused_session) => {
            tracing::debug!(target_session = %name, "switch-session to current session; no-op");
        }
        ReattachTarget::Existing {
            name,
            id,
            window,
            pane,
            resource,
        } => {
            tracing::info!(target_session = %name, target_id = ?id, target_window = ?window, target_pane = ?pane, target_resource = ?resource, "switch-session requested");
            *switch_request = Some(ReattachTarget::Existing {
                name,
                id,
                window,
                pane,
                resource,
            });
        }
        ReattachTarget::Create {
            name,
            directory,
            host,
        } => {
            tracing::info!(session = %name, directory = ?directory, host = ?host, "new-session requested");
            *switch_request = Some(ReattachTarget::Create {
                name,
                directory,
                host,
            });
        }
    }
}

fn is_current_session(
    id: Option<SessionId>,
    name: &str,
    session_name: &str,
    focused_session: Option<SessionId>,
) -> bool {
    id.map_or_else(|| name == session_name, |id| Some(id) == focused_session)
}

/// rename-session via the shared [`phux_client::rename`] policy, correlated
/// by the driver rather than awaited here (a request/response helper would
/// eat interleaved output). A refusal is parked on `rename_notice`; the
/// status name changes only once the server confirms.
///
/// Against an `ORGANIZATION` server the rename is one `SESSION_RENAME`
/// naming the attached session's key, settled by its reply. Against an
/// older server it is a `SET_METADATA` of the translated name key followed
/// by a `GET_STATE` barrier whose snapshot is the outcome.
#[allow(
    clippy::future_not_send,
    reason = "DispatchCtx holds current-thread state"
)]
async fn send_session_rename(
    rename_session: Option<String>,
    conn: &mut Connection,
    ctx: &mut DispatchCtx<'_>,
) -> Result<(), AttachError> {
    let Some(new_name) = rename_session.filter(|n| n != ctx.session_name.as_str()) else {
        return Ok(());
    };
    if ctx.rename_pending.is_some() {
        tracing::warn!("rename-session already in flight; dropping a second request");
        return Ok(());
    }
    let session_name = ctx.session_name.clone();
    let roster: Vec<_> = ctx
        .peers
        .sessions
        .iter()
        .map(|session| phux_client::rename::NamedSession {
            id: session.id,
            name: session.name.as_str(),
        })
        .collect();
    let session_id = match phux_client::rename::plan_rename(&roster, &session_name, &new_name) {
        phux_client::rename::RenamePlan::Unchanged { .. } => return Ok(()),
        phux_client::rename::RenamePlan::Refused(refusal) => {
            *ctx.rename_notice = Some(format!("could not rename session to {new_name}: {refusal}"));
            return Ok(());
        }
        phux_client::rename::RenamePlan::Send { session_id } => session_id,
    };
    let session_key = ctx
        .peers
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .and_then(|session| session.key);
    if ctx.organization_supported
        && let Some(key) = session_key
    {
        let request_id = ctx.take_request_id();
        let command = phux_client::rename::org_rename_command(
            phux_client::org::mint_operation(),
            key,
            &new_name,
        );
        conn.send(&FrameKind::Command {
            request_id,
            command,
        })
        .await?;
        tracing::info!(new_name = %new_name, "rename-session sent as SESSION_RENAME");
        *ctx.rename_pending = Some(PendingSessionRename {
            barrier: request_id,
            session_id: Some(session_id),
            current: session_name,
            new_name,
            organization: true,
        });
        return Ok(());
    }
    let request_id = ctx.take_request_id();
    let barrier = ctx.take_request_id();
    let (write, barrier_frame) =
        phux_client::rename::rename_frames(request_id, barrier, &session_name, &new_name);
    conn.send(&write).await?;
    conn.send(&barrier_frame).await?;
    tracing::info!(new_name = %new_name, "rename-session sent; waiting for GET_STATE confirmation");
    *ctx.rename_pending = Some(PendingSessionRename {
        barrier,
        session_id: Some(session_id),
        current: session_name,
        new_name,
        organization: false,
    });
    Ok(())
}

/// Result of feeding a key event through the resolver.
pub(super) enum ChordOutcome {
    /// Chord extended a partial sequence; absorb and wait.
    Partial,
    /// Chord completed a binding; effects follow.
    Resolved(phux_config::keybind::ResolvedAction),
}

/// Feed a key press to the resolver (releases and repeats are ignored, so
/// held keys do not re-fire). `None` without a resolver or on no match.
pub(super) fn consume_chord(
    ctx: &mut DispatchCtx<'_>,
    key_event: &phux_protocol::input::key::KeyEvent,
) -> Option<ChordOutcome> {
    use phux_protocol::input::key::{KeyAction, ModSet};
    let resolver = ctx.resolver.as_deref_mut()?;
    if !matches!(key_event.action, KeyAction::Press) {
        return None;
    }
    let chord = phux_config::keybind::KeyChord {
        // A kitty-mode host reports Caps/Num Lock on `CSI u` keys (Ctrl+A is
        // `CSI 97;69u` with Caps Lock on); bindings name held modifiers only.
        modifiers: key_event.mods - (ModSet::CAPS_LOCK | ModSet::NUM_LOCK),
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
    /// `true` ⇒ flip the driver's pane-zoom state (zoom the
    /// focused pane to fill the window, or un-zoom). `apply_action_effects`
    /// owns the actual toggle since the `zoomed` state lives in the driver.
    pub(super) toggle_zoom: bool,
    /// Flip the driver's sidebar state (and reflow).
    pub(super) toggle_sidebar: bool,
    /// ADR-0168: a project group key whose sidebar collapse to flip.
    pub(super) toggle_project: Option<String>,
    /// The new focused pane, already applied to the active window.
    pub(super) set_focus: Option<ResourceId>,
    /// `true` ⇒ emit `SET_METADATA` carrying the new layout envelope.
    pub(super) set_metadata: bool,
    /// `true` ⇒ emit a terminal bell (BEL `\x07`).
    pub(super) bell: bool,
    /// The active window changed: drop the prediction queue.
    pub(super) clear_predict: bool,
    /// `true` ⇒ emit `DETACH` and wait for `DETACHED`.
    pub(super) detach: bool,
    /// A parked `split-pane` spawn: send, then park in `pending_splits`.
    pub(super) spawn_terminal: Option<(u32, PendingSplit, FrameKind)>,
    /// A parked `new-window` spawn: send, then park in `pending_windows`.
    pub(super) spawn_window: Option<(u32, PendingWindow, FrameKind)>,
    /// ADR-0147: a floating plugin overlay spawn `(request, title, frame)`:
    /// send, then park the title in `pending_floating`.
    pub(super) spawn_floating: Option<(u32, String, FrameKind)>,
    /// A picker-confirmed existing-pane move. The async effect applier opens a
    /// dedicated control connection and runs the canonical headless operation.
    pub(super) move_pane: Option<PaneMoveIntent>,
    /// A session management request, prepared or confirmed on a control connection.
    pub(super) session_management: Option<phux_config::keybind::ResolvedAction>,
    /// Background work to start off the input loop (ADR-0172): a plugin
    /// action or an organize run. Its result returns as a toast.
    pub(super) job: Option<Job>,
    /// A `go-to-directory` action built a `LIST_DIRECTORY` request. The
    /// async caller records it (id and listed host) as the pending listing,
    /// then sends it; the reply opens the directory picker.
    pub(super) list_directory: Option<(PendingDirectory, FrameKind)>,
    pub(super) query_path: Option<(crate::attach::path_picker::PendingPath, FrameKind)>,
    pub(super) insert_path: Option<(ResourceId, String)>,
    /// `KILL_RESOURCE` commands, one per targeted Terminal, sent in order.
    pub(super) kill_frames: Vec<FrameKind>,
    /// `(request_id, Terminal)` per kill, parked in `pending_kills` so a
    /// `TerminalNotFound` refusal can fold a dead leaf out.
    pub(super) kill_requests: Vec<(u32, ResourceId)>,
    /// The kill targets, parked in `expected_closes` to silence their notices.
    pub(super) expected_closes: Vec<ResourceId>,
    /// ADR-0033 supervisory commands for the focused pane, sent in order.
    pub(super) command_frames: Vec<FrameKind>,
    /// An in-process re-attach (switch or create) for the driver.
    pub(super) reattach: Option<ReattachTarget>,
    /// ADR-0140: `switch-host { host, name }` committed. Unlike
    /// [`Self::reattach`] this leaves the server: the batch detaches, then
    /// the process becomes `phux attach` against that machine
    /// ([`crate::attach::hosts::exec_switch_host`]). `(host, session)`.
    pub(super) switch_host: Option<(String, String)>,
    /// rename-session: a committed rename. Carries the new name. The async
    /// caller ([`apply_action_effects`]) sends a `SESSION_RENAME` for the
    /// *current* session against an `ORGANIZATION` server, else a
    /// `SET_METADATA` write of `SESSION_NAME_KEY` plus a correlated
    /// `GET_STATE` barrier, and parks [`PendingSessionRename`] so the driver
    /// can apply or refuse from the reply. The local status name stays
    /// unchanged until that confirmation (or the `SESSION_RENAMED` event or
    /// `METADATA_CHANGED` broadcast) so a rejected rename cannot lie. Peers
    /// learn the new name from the same event or broadcast rather than
    /// waiting for a re-attach.
    pub(super) rename_session: Option<String>,
    /// A requested live config reload, run by the driver after the batch.
    pub(super) reload_config: bool,
    /// OSC 52 clipboard payload to emit on the host terminal after this
    /// action (the `report-bug` path copies the bundle path).
    pub(super) clipboard: Option<String>,
}

/// Exact move selected by the fuzzy destination picker.
#[derive(Debug)]
pub(super) struct PaneMoveIntent {
    pub(super) source: ResourceId,
    pub(super) target: ResourceId,
    pub(super) dir: SplitDir,
    pub(super) ratio: f32,
}

/// An in-process re-attach raised by `switch-session`/`new-session`; the
/// outer loop re-attaches on the same connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReattachTarget {
    /// Switch to an existing session (`switch-session`).
    Existing {
        /// Target session display name. Used when `id` is absent (a typed
        /// name or a satellite hop) and as the human-facing log label.
        name: String,
        /// Stable session identity from a painted row (a rename cannot
        /// retarget it); `None` for satellite rows.
        id: Option<SessionId>,
        /// Window to select once the target's layout loads; out of range
        /// is a logged no-op.
        window: Option<usize>,
        /// DFS leaf ordinal within `window` to focus, likewise.
        pane: Option<usize>,
        /// Pane identity to focus after re-attach, instead of indices.
        resource: Option<ResourceId>,
    },
    /// Create — or attach to, if it already exists — a session by name
    /// (`new-session`). `directory` and `host` come from the focused pane
    /// or an explicit pick, never from the client process's working directory.
    Create {
        /// Session name.
        name: String,
        /// Seed-pane directory. `None` leaves the server default.
        directory: Option<String>,
        /// Satellite host. `None` creates on the attached server.
        host: Option<String>,
    },
}

/// An in-flight `rename-session` waiting on its `SESSION_RENAME` reply, or
/// on its `GET_STATE` barrier (`SET_METADATA` has no reply).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::attach) struct PendingSessionRename {
    /// Request id of the `SESSION_RENAME`, or of the confirming `GET_STATE`.
    pub barrier: u32,
    /// The attached session's id when the rename was sent, if known.
    pub session_id: Option<SessionId>,
    /// Display name at send time (`current` in `current\0new`).
    pub current: String,
    /// Requested replacement name.
    pub new_name: String,
    /// Sent as `SESSION_RENAME` (ADR-0161): the reply is `COMMITTED` or
    /// `REFUSED`, not a snapshot.
    pub organization: bool,
}

/// Encode the workspace for `SET_METADATA`, logging encode failures.
/// Returns `None` on failure — caller should not emit a frame in that case.
pub(in crate::attach) fn encode_layout_or_log(workspace: &ArrangementDoc) -> Option<Vec<u8>> {
    match workspace.encode_cbor() {
        Ok(bytes) => Some(bytes),
        Err(err) => {
            tracing::warn!(error = %err, "layout CBOR encode failed; SET_METADATA skipped");
            None
        }
    }
}
