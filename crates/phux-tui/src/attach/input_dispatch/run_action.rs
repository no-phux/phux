//! The action interpreter: one arm per canonical action name, plus the
//! action-finder overlay push.

use std::collections::HashMap;

use phux_protocol::ResourceId;
use phux_protocol::ids::SatelliteHost;
use phux_protocol::wire::frame::{Command, FrameKind, InputMode};

use crate::attach::actions::{self, ActionError, Adopt, PendingSplit, PendingWindow, SplitHost};
use crate::attach::copy::extract_selection_text;
use crate::attach::directory_picker::{DirectorySupport, ListingHost, PendingDirectory};
use crate::attach::pane_state::{PaneSlot, published_terminal};
use crate::attach::plugin_panes::HostedPlacement;
use crate::layout::{LayoutState, SplitDir, Workspace};
use crate::render::overlay::{
    CopyRequest, PendingOverlay, PromptOverlay, SelectItem, SelectList, SelectionGrab, ToastOverlay,
};
use phux_client::layout_ops::DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID;

use super::args::{
    PaneMouseArg, amount_arg, direction_arg, focus_terminal, index_arg, kill_resource_frame,
    mouse_arg, name_arg, ordered_workspace_panes, resource_id_arg, session_id_arg, signal_arg,
    split_dir_arg, str_arg, usize_arg,
};
use super::ctx::DispatchCtx;
use super::dispatch::{
    focused_pane_rect, open_context_menu, predicted_split_size, set_spawn_initial_size,
    spawn_initial_size, terminal_in_alt_screen, terminal_wants_mouse_tracking,
};
use super::effects::{ActionEffects, PaneMoveIntent, ReattachTarget};
use super::pickers::{
    SESSION_PICKER_LIVE_KEY, move_pane_picker_items, session_picker_rows, switch_window,
    window_holding, window_picker_items,
};

/// Open the single fuzzy discovery surface. `show-help` and
/// `command-palette` are entry aliases so users never have to choose between
/// a reference modal and an executable finder.
pub(super) fn push_action_finder(ctx: &mut DispatchCtx<'_>) {
    let items = crate::attach::action_registry::palette_items(
        ctx.keybindings,
        ctx.plugin_actions,
        ctx.plugin_panes,
    );
    ctx.overlays
        .push(Box::new(SelectList::new("Commands", items, ctx.theme)));
}

/// Dispatch a resolved action: one arm per canonical action name. Sync by
/// design; the caller does the frame I/O, so a send failure never leaves
/// layout state half-mutated.
pub(super) fn run_action(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    // Read-only pane slots (the fleet reads them); passed apart from `ctx`
    // because the driver lends `panes` mutably beside it.
    panes: &HashMap<ResourceId, PaneSlot>,
) -> ActionEffects {
    tracing::info!(action = %resolved.action, "input: running resolved action");
    let mut effects = ActionEffects::default();
    let e = &mut effects;
    match resolved.action.as_str() {
        "split-pane" => split_pane(resolved, ctx, focused, panes, e),
        "move-pane" => move_pane(resolved, ctx, focused, e),
        "kill-pane" => kill_focused_pane(ctx, focused, e),
        "take-input" => take_input(ctx, focused, e),
        "give-input" => give_input(ctx, focused, e),
        "signal-terminal" => signal_terminal(resolved, ctx, focused, e),
        "set-pane" => set_pane(resolved, ctx, focused, e),
        "new-window" => new_window(resolved, ctx, focused, panes, e),
        "go-to-directory" => go_to_directory(resolved, ctx, focused, panes, e),
        "find-path" => find_path(resolved, ctx, focused, panes, e),
        "insert-path" => insert_path(resolved, ctx, focused, panes, e),
        "kill-window" => kill_active_window(ctx, e),
        "next-window" => switch_window(ctx, e, Workspace::next),
        "previous-window" => switch_window(ctx, e, Workspace::prev),
        "select-window" => select_window(resolved, ctx, e),
        "move-window" => move_window(resolved, ctx, e),
        "rename-window" => rename_window(resolved, ctx, e),
        "rename-session" => rename_session(resolved, ctx, e),
        "focus-direction" => focus_direction(resolved, ctx, e),
        "resize-pane" => resize_pane(resolved, ctx, e),
        "reload-config" => reload_config(e),
        "show-help" | "command-palette" => push_action_finder(ctx),
        "getting-started" => push_getting_started(ctx),
        "settings" => push_settings(ctx),
        "report-bug" => report_bug(resolved, ctx, focused, e),
        "copy-mode" => push_copy_mode(ctx, focused),
        "context-menu" => push_context_menu(ctx, focused),
        "window-picker" => push_window_picker(ctx, e),
        "session-picker" => push_session_picker(ctx),
        "agent-fleet" => push_agent_fleet(ctx, panes, e),
        "next-attention" => next_attention(ctx, focused, panes, e),
        "return-from-attention" => return_from_attention(ctx, e),
        "focus-pane" => focus_pane(resolved, ctx, e),
        "switch-session" => switch_session(resolved, ctx, e),
        "switch-host" => switch_host(resolved, e),
        "new-session" => new_session(resolved, ctx, focused, panes, e),
        "last-session" => last_session(ctx, e),
        "next-session" => step_session(ctx, e, 1),
        "previous-session" => step_session(ctx, e, -1),
        "detach" => e.detach = true,
        "plugin-action" => plugin_action(resolved, e),
        "plugin-pane" => plugin_pane(resolved, ctx, focused, e),
        "next-pane" => cycle_pane(ctx, e, actions::apply_next_pane),
        "previous-pane" => cycle_pane(ctx, e, actions::apply_previous_pane),
        "last-pane" => last_pane(ctx, focused, e),
        "toggle-zoom" => toggle_zoom(ctx, e),
        "toggle-sidebar" => toggle_sidebar(ctx, e),
        other => {
            tracing::debug!(action = other, "unhandled resolved action");
        }
    }
    effects
}

/// Open the all-session destination picker, or commit its exact selected row.
/// Side-by-side at 0.5 is the single keyboard-fast placement policy.
fn move_pane(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    effects: &mut ActionEffects,
) {
    let Some(source) = focused.cloned() else {
        effects.bell = true;
        return;
    };
    if !matches!(source, ResourceId::Local { .. }) {
        tracing::warn!("move-pane: satellite source panes are not supported");
        effects.bell = true;
        return;
    }

    let Some(target) = resolved.args.get("target") else {
        let items = move_pane_picker_items(
            &source,
            ctx.workspace,
            ctx.session_name,
            ctx.peers.focused_session,
            ctx.peers.sessions,
            ctx.peers.foreign_layouts,
        );
        if items.is_empty() {
            effects.bell = true;
            return;
        }
        ctx.overlays.push(Box::new(SelectList::new(
            "Move pane beside…",
            items,
            ctx.theme,
        )));
        return;
    };

    let Some(id) = target.as_integer().and_then(|id| u32::try_from(id).ok()) else {
        effects.bell = true;
        return;
    };
    effects.move_pane = Some(PaneMoveIntent {
        source,
        target: ResourceId::Local { id },
        dir: SplitDir::Horizontal,
        ratio: 0.5,
    });
}

/// Park a `PendingSplit` and send `SPAWN_RESOURCE`; the reply handler splits.
/// A satellite pane splits on its satellite at its cwd, applying once the
/// relayed pane's attach succeeds; see [`split_host`] for older hubs.
fn split_pane(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    panes: &HashMap<ResourceId, PaneSlot>,
    effects: &mut ActionEffects,
) {
    let Some(dir) = split_dir_arg(resolved) else {
        tracing::warn!(
            args = ?resolved.args,
            "split-pane missing/bad `direction` arg (expected horizontal|vertical)",
        );
        effects.bell = true;
        return;
    };
    let Some(focused_id) = focused.cloned() else {
        tracing::warn!("split-pane: no focused pane to split against; dropping action");
        effects.bell = true;
        return;
    };
    // `resource = "host/@N"` (or `@N`) opens that existing
    // pane into this window. A spawn is the other shape, below.
    if resolved.args.contains_key("resource") {
        let Some(target) = resource_id_arg(resolved) else {
            tracing::warn!(
                args = ?resolved.args,
                "split-pane `resource` is not `@N` or `host/@N`",
            );
            effects.bell = true;
            return;
        };
        open_existing_pane(ctx, effects, focused_id, dir, target);
        return;
    }
    let request_id = ctx.take_request_id();
    // `split-pane { host }` spawns there with no owner; else follow the pane.
    let place = spawn_place(resolved, Some(&focused_id), panes);
    let wanted = place.host.clone().map(SatelliteHost::new);
    let host = split_host(wanted, ctx.directory_support);
    let satellite = match &host {
        SplitHost::Satellite(satellite) => Some(satellite.clone()),
        SplitHost::Attached | SplitHost::AttachedInsteadOf(_) => None,
    };
    // A directory is only sent to the host that owns it. An older hub that
    // cannot spawn on the satellite must not receive that satellite's path.
    let cwd = match &host {
        SplitHost::Satellite(_) => place.directory,
        SplitHost::Attached if place.host.is_none() => place.directory,
        SplitHost::Attached | SplitHost::AttachedInsteadOf(_) => None,
    };
    let pending = PendingSplit {
        focused_at_request: focused_id,
        dir,
        zoom_on_spawn: false,
        host,
        adopt: None,
        open_existing: None,
    };
    let mut frame = FrameKind::SpawnResource {
        request_id,
        group: DEFAULT_GROUP_ID,
        command: None,
        cwd,
        env: None,
        term: None,
        satellite,
        owner_terminal: None,
        agent_session: None,
        initial_size: predicted_split_size(ctx, &pending),
        resource: None,
    };
    bind_satellite_spawn(&mut frame);
    effects.spawn_terminal = Some((request_id, pending, frame));
}

/// ADR-0109: ask a satellite spawn to bind to the satellite's instance
/// token, so a stranded pane can later be killed conditionally. Peers
/// without `CONDITIONAL_KILL` skip the field; local spawns are unchanged.
fn bind_satellite_spawn(frame: &mut FrameKind) {
    if matches!(
        frame,
        FrameKind::SpawnResource {
            satellite: Some(_),
            ..
        }
    ) {
        phux_client::conditional_kill::request_binding(frame);
    }
}

/// Attach an existing `target` and split it into the current window once
/// the attach succeeds (a pane already here is focused instead). A refusal
/// never kills a pane this client did not spawn.
fn open_existing_pane(
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
    focused_id: ResourceId,
    dir: SplitDir,
    target: ResourceId,
) {
    if let Some(index) = window_holding(ctx.workspace, &target) {
        focus_in_window(ctx, effects, index, target);
        return;
    }
    if attach_in_flight(ctx.pending_windows, &target)
        || ctx.pending_splits.values().any(|split| {
            split.open_existing.as_ref() == Some(&target)
                || split.adopt.as_ref().map(|spawned| &spawned.id) == Some(&target)
        })
    {
        return;
    }
    let request_id = ctx.take_request_id();
    let host = split_host(target.host().cloned(), ctx.directory_support);
    let pending = PendingSplit {
        focused_at_request: focused_id,
        dir,
        zoom_on_spawn: false,
        host,
        adopt: None,
        open_existing: Some(target.clone()),
    };
    effects.spawn_terminal = Some((request_id, pending, attach_frame(request_id, target)));
}

/// `ATTACH_RESOURCE` for an existing pane.
fn attach_frame(request_id: u32, terminal_id: ResourceId) -> FrameKind {
    FrameKind::Command {
        request_id,
        command: Command::AttachResource {
            terminal_id,
            role_policy: crate::attach::attach_role::pane_attach_role(),
        },
    }
}

/// Where a split onto `host` (`None`: the attached server) spawns. Only a
/// hub advertising `LIST_DIRECTORY_HOST` (shipped with host-aware spawns)
/// honors `SPAWN_RESOURCE.satellite`; an older one would skip the field and
/// spawn on itself, so the split stays there and says so.
fn split_host(host: Option<SatelliteHost>, support: DirectorySupport) -> SplitHost {
    match host {
        None => SplitHost::Attached,
        Some(host) if support == DirectorySupport::HostAware => SplitHost::Satellite(host),
        Some(host) => SplitHost::AttachedInsteadOf(host),
    }
}

/// Close the focused pane with one correlated `KILL_RESOURCE`. A
/// `TerminalNotFound` refusal folds the leaf out too, so a pane whose
/// resource died under us can still be dismissed.
fn kill_focused_pane(
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    effects: &mut ActionEffects,
) {
    let Some(focused_id) = focused.cloned() else {
        tracing::warn!("kill-pane: no focused pane to kill; dropping action");
        effects.bell = true;
        return;
    };
    let request_id = ctx.take_request_id();
    effects.kill_frames = vec![kill_resource_frame(&focused_id, request_id)];
    effects.kill_requests = vec![(request_id, focused_id.clone())];
    // Mark the close as ours so the resulting
    // RESOURCE_CLOSED does not raise a pane-exit notice.
    effects.expected_closes = vec![focused_id];
}

/// Send `command(focused pane)` as a correlated `COMMAND`, or bell with no
/// focused pane.
fn focused_command(
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    effects: &mut ActionEffects,
    command: impl FnOnce(ResourceId) -> Command,
) {
    let Some(focused_id) = focused.cloned() else {
        tracing::warn!("no focused pane; dropping action");
        effects.bell = true;
        return;
    };
    let request_id = ctx.take_request_id();
    effects.command_frames.push(FrameKind::Command {
        request_id,
        command: command(focused_id),
    });
}

/// ADR-0033: seize the focused pane's input lease (preempting any holder).
fn take_input(
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    effects: &mut ActionEffects,
) {
    focused_command(ctx, focused, effects, |terminal_id| Command::AcquireInput {
        terminal_id,
        mode: InputMode::Seize,
        ttl_ms: 0,
    });
}

/// ADR-0033: release the focused pane's input lease.
fn give_input(
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    effects: &mut ActionEffects,
) {
    focused_command(ctx, focused, effects, |terminal_id| Command::ReleaseInput {
        terminal_id,
    });
}

/// ADR-0033: deliver a POSIX signal to the focused pane's process
/// group. `freeze`/`resume` is the reversible brake; distinct from
/// `kill-pane`, which removes the pane.
fn signal_terminal(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    effects: &mut ActionEffects,
) {
    let Some(signal) = signal_arg(resolved) else {
        tracing::warn!(
            args = ?resolved.args,
            "signal-terminal missing/bad `signal` arg (interrupt|freeze|resume|terminate|kill)",
        );
        effects.bell = true;
        return;
    };
    focused_command(ctx, focused, effects, |terminal_id| {
        Command::SignalTerminal {
            terminal_id,
            signal,
            operation_id: None,
        }
    });
}

/// Flip the focused pane's client-local mouse opt-out (ADR-0048): off means
/// no synthesized `INPUT_MOUSE` and outer capture dropped while focused.
fn set_pane(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    effects: &mut ActionEffects,
) {
    let Some(mode) = mouse_arg(resolved) else {
        tracing::warn!(
            args = ?resolved.args,
            "set-pane missing/bad `mouse` arg (expected on|off|toggle or a bool)",
        );
        effects.bell = true;
        return;
    };
    let Some(focused_id) = focused.cloned() else {
        tracing::warn!("set-pane: no focused pane; dropping action");
        effects.bell = true;
        return;
    };
    let opt_out = match mode {
        PaneMouseArg::Off => true,
        PaneMouseArg::On => false,
        PaneMouseArg::Toggle => !ctx.mouse_optout.contains(&focused_id),
    };
    if opt_out {
        ctx.mouse_optout.insert(focused_id.clone());
    } else {
        ctx.mouse_optout.remove(&focused_id);
    }
    tracing::info!(
        terminal = ?focused_id,
        mouse = !opt_out,
        "set-pane: per-pane mouse opt-out updated"
    );
    // No repaint needed: the opt-out has no chrome today, and the
    // driver re-syncs the outer capture DECSET from this set at the
    // top of every loop iteration.
}

/// Open a new window: spawn a Terminal and park a `PendingWindow`; the reply
/// opens and activates the window. `cwd` starts the shell there; `host`
/// spawns on that satellite through the hub (`cwd` is then a satellite path).
fn new_window(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    panes: &HashMap<ResourceId, PaneSlot>,
    effects: &mut ActionEffects,
) {
    let request_id = ctx.take_request_id();
    let name = ctx.workspace.default_window_name();
    let place = spawn_place(resolved, focused, panes);
    let mut frame = FrameKind::SpawnResource {
        request_id,
        group: DEFAULT_GROUP_ID,
        command: None,
        cwd: place.directory,
        env: None,
        term: None,
        satellite: place.host.map(SatelliteHost::new),
        owner_terminal: None,
        agent_session: None,
        // The new window holds one leaf, so the pane
        // fills the whole content rect. Predicting that here spares
        // the pane a bootstrap-then-reflow round trip.
        initial_size: spawn_initial_size(ctx, |content| Some((content.w, content.h))),
        resource: None,
    };
    bind_satellite_spawn(&mut frame);
    effects.spawn_window = Some((request_id, PendingWindow { name, adopt: None }, frame));
}

/// Request a directory listing (`docs/spec/L3.md` §4) on [`listing_host`],
/// starting at `path`, else the focused pane's cwd when it lives on that
/// host, else home. Bells when the server lacks `LIST_DIRECTORY`.
fn go_to_directory(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    panes: &HashMap<ResourceId, PaneSlot>,
    effects: &mut ActionEffects,
) {
    if ctx.directory_support == DirectorySupport::Unsupported {
        tracing::warn!(
            "go-to-directory: server does not advertise LIST_DIRECTORY; dropping action"
        );
        effects.bell = true;
        return;
    }
    let host = listing_host(host_arg(resolved), focused, ctx.directory_support);
    let path = str_arg(resolved, "path")
        .or_else(|| pane_cwd_on(host.satellite(), focused, panes))
        .unwrap_or_default();
    let request_id = ctx.take_request_id();
    // Modal from the moment the request leaves: the placeholder swallows
    // keystrokes and Escape cancels, so nothing typed during a slow listing
    // reaches the pane and a cancelled listing never opens late.
    ctx.overlays.push(Box::new(PendingOverlay::listing(
        &placeholder_label(&host, &path),
        request_id,
        ctx.theme,
    )));
    effects.layout_mutated = true;
    let frame = FrameKind::ListDirectory {
        request_id,
        path,
        host: host.satellite().cloned(),
    };
    effects.list_directory = Some((PendingDirectory { request_id, host }, frame));
}

/// Browse a host path while preserving the pane captured at first open.
fn find_path(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    panes: &HashMap<ResourceId, PaneSlot>,
    effects: &mut ActionEffects,
) {
    if !ctx.path_query_supported {
        effects.bell = true;
        return;
    }
    let Some(target) = focused else {
        effects.bell = true;
        return;
    };
    // A browse row must never recapture a different pane when focus or its
    // input lease moved during the overlay's lifetime.
    let old = ctx.pending_path.as_ref();
    if old.is_some_and(|pending| {
        !crate::attach::path_picker::may_insert(pending, focused, ctx.own_client_id, panes)
    }) {
        *ctx.pending_path = None;
        effects.bell = true;
        return;
    }
    let holder = old.map_or_else(
        || panes.get(target).and_then(|p| p.input_holder),
        |p| p.holder,
    );
    if holder.is_some_and(|id| Some(id) != ctx.own_client_id) || panes.get(target).is_none() {
        effects.bell = true;
        return;
    }
    let root = str_arg(resolved, "path")
        .or_else(|| pane_cwd_on(target.host(), focused, panes))
        .unwrap_or_default();
    let request_id = ctx.take_request_id();
    let pending = crate::attach::path_picker::PendingPath {
        target: target.clone(),
        holder,
        request_id,
        root: root.clone(),
        query: String::new(),
    };
    ctx.overlays
        .push(Box::new(crate::render::overlay::PathPicker::new(
            root.clone(),
            ctx.theme,
        )));
    effects.layout_mutated = true;
    effects.query_path = Some((
        pending,
        FrameKind::PathQuery {
            request_id,
            root,
            query: String::new(),
            recursive: false,
            host: target.host().cloned(),
        },
    ));
}

fn insert_path(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    panes: &HashMap<ResourceId, PaneSlot>,
    effects: &mut ActionEffects,
) {
    let Some(pending) = ctx.pending_path.take() else {
        effects.bell = true;
        return;
    };
    let Some(path) = str_arg(resolved, "path") else {
        effects.bell = true;
        return;
    };
    if !path.starts_with('/')
        || path.chars().any(char::is_control)
        || !crate::attach::path_picker::may_insert(&pending, focused, ctx.own_client_id, panes)
    {
        effects.bell = true;
        return;
    }
    effects.insert_path = Some((
        pending.target,
        crate::attach::path_picker::shell_quote(&path),
    ));
}

/// A non-empty `host` arg as a satellite name.
fn host_arg(resolved: &phux_config::keybind::ResolvedAction) -> Option<SatelliteHost> {
    str_arg(resolved, "host")
        .filter(|host| !host.is_empty())
        .map(SatelliteHost::new)
}

/// The host one listing reads: `wanted`, else the focused pane's satellite,
/// else the attached server. An older hub would list itself, so the request
/// stays there and the picker says so.
fn listing_host(
    wanted: Option<SatelliteHost>,
    focused: Option<&ResourceId>,
    support: DirectorySupport,
) -> ListingHost {
    let wanted = wanted.or_else(|| focused.and_then(ResourceId::host).cloned());
    match wanted {
        None => ListingHost::Attached,
        Some(host) if support == DirectorySupport::HostAware => ListingHost::Satellite(host),
        Some(host) => ListingHost::AttachedInsteadOf(host),
    }
}

/// The focused pane's working directory, when that pane lives on `host`
/// (`None` is the attached server). A pane on any other host names a path
/// the listed host does not have, so the listing starts at home instead.
fn pane_cwd_on(
    host: Option<&SatelliteHost>,
    focused: Option<&ResourceId>,
    panes: &HashMap<ResourceId, PaneSlot>,
) -> Option<String> {
    let focused = focused.filter(|id| id.host() == host)?;
    panes.get(focused)?.cwd.clone()
}

/// What the "Listing ..." placeholder names: the path (`~` for home), and
/// the satellite when the listing is relayed to one.
fn placeholder_label(host: &ListingHost, path: &str) -> String {
    let shown = if path.is_empty() { "~" } else { path };
    host.satellite()
        .map_or_else(|| shown.to_owned(), |host| format!("{shown} on {host}"))
}

/// Close every pane in the active window, one correlated `KILL_RESOURCE`
/// each; the closes fold the window away.
fn kill_active_window(ctx: &mut DispatchCtx<'_>, effects: &mut ActionEffects) {
    let leaves = ctx
        .workspace
        .active_window()
        .and_then(|ls| ls.tree.as_ref().map(crate::layout::leaves))
        .unwrap_or_default();
    if leaves.is_empty() {
        tracing::warn!("kill-window: no active window to kill; dropping action");
        effects.bell = true;
        return;
    }
    effects.kill_requests = leaves
        .iter()
        .map(|leaf| (ctx.take_request_id(), leaf.clone()))
        .collect();
    effects.kill_frames = effects
        .kill_requests
        .iter()
        .map(|(request_id, leaf)| kill_resource_frame(leaf, *request_id))
        .collect();
    // Every pane in the window dies at our request;
    // none of those closes is news.
    effects.expected_closes = leaves;
}

/// Switch the client-local active window to an explicit `index` arg.
fn select_window(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
) {
    let Some(index) = index_arg(resolved) else {
        tracing::warn!(args = ?resolved.args, "select-window missing/bad `index` arg");
        effects.bell = true;
        return;
    };
    switch_window(ctx, effects, |w| {
        w.select(index);
    });
}

/// Move the active window to `index`, else `delta` slots along, clamped.
/// It stays active; order is shared state, so the move broadcasts.
fn move_window(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
) {
    let from = ctx.workspace.active;
    let last = ctx.workspace.windows.len().saturating_sub(1);
    let target = index_arg(resolved)
        .map(|index| index.min(last))
        .or_else(|| {
            let delta = resolved.args.get("delta")?.as_integer()?;
            Some(offset_index(from, delta, ctx.workspace.windows.len()))
        });
    let Some(to) = target else {
        tracing::warn!(args = ?resolved.args, "move-window needs `index` or `delta`");
        effects.bell = true;
        return;
    };
    if !ctx.workspace.move_window(from, to) {
        effects.bell = true;
        return;
    }
    effects.layout_mutated = true;
    effects.set_metadata = true;
}

/// `from` moved `delta` slots, clamped to `0..len`.
fn offset_index(from: usize, delta: i64, len: usize) -> usize {
    let last = len.saturating_sub(1);
    let magnitude = usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX);
    if delta < 0 {
        from.saturating_sub(magnitude)
    } else {
        from.saturating_add(magnitude).min(last)
    }
}

/// Rename the active window, directly or through the interactive prompt.
fn rename_window(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
) {
    if ctx.workspace.active_window().is_none() {
        tracing::warn!("rename-window: no active window; dropping action");
        effects.bell = true;
        return;
    }
    if let Some(name) = name_arg(resolved) {
        // Explicit `name` renames immediately. A rename is shared
        // window state, so (unlike focus/switch) it broadcasts.
        ctx.workspace.rename_active(name);
        effects.layout_mutated = true;
        effects.set_metadata = true;
    } else {
        // No name ⇒ open the interactive prompt pre-filled with
        // the active window's current name. On commit it re-runs
        // `rename-window` with the typed name.
        let current = ctx
            .workspace
            .windows
            .get(ctx.workspace.active)
            .map(|w| w.name.clone())
            .unwrap_or_default();
        ctx.overlays
            .push(Box::new(PromptOverlay::rename_window(&current, ctx.theme)));
        effects.layout_mutated = true;
    }
}

/// Rename the attached session (`name`), or prompt for a name that commits
/// back through here. The send happens in `apply_action_effects`.
fn rename_session(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
) {
    if let Some(name) = name_arg(resolved) {
        effects.rename_session = Some(name);
    } else {
        ctx.overlays.push(Box::new(PromptOverlay::rename_session(
            ctx.session_name,
            ctx.theme,
        )));
        effects.layout_mutated = true;
    }
}

/// Move focus to the neighbouring pane in the requested direction.
fn focus_direction(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
) {
    let Some(dir) = direction_arg(resolved) else {
        tracing::warn!(args = ?resolved.args, "focus-direction missing/bad `direction` arg");
        effects.bell = true;
        return;
    };
    // No neighbour: silently drop (tmux: the layout edge isn't a bell).
    cycle_pane(ctx, effects, |ls| actions::apply_focus(ls, dir));
}

/// Move the focused pane's boundary by `amount` along `direction`.
fn resize_pane(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
) {
    let (Some(dir), Some(amount)) = (direction_arg(resolved), amount_arg(resolved)) else {
        tracing::warn!(args = ?resolved.args, "resize-pane missing args");
        effects.bell = true;
        return;
    };
    let Some(ls) = ctx.workspace.active_window_mut() else {
        effects.bell = true;
        return;
    };
    match actions::apply_resize(ls, dir, amount, ctx.viewport, ctx.sidebar) {
        Ok(Some(new_state)) => {
            *ls = new_state;
            effects.layout_mutated = true;
            effects.set_metadata = true;
        }
        Ok(None) | Err(ActionError::NoResizableBoundary) => {
            // Underflow guard tripped or no matching axis —
            // bell-no-op (ADR-0019 decision 5).
            effects.bell = true;
        }
        Err(err) => {
            tracing::warn!(error = %err, "resize-pane failed");
            effects.bell = true;
        }
    }
}

/// Explicit reload; the driver re-reads after the batch because `ctx`
/// borrows exactly the state a reload replaces.
const fn reload_config(effects: &mut ActionEffects) {
    effects.reload_config = true;
}

/// ADR-0101: open the settings page; a saved edit returns as `reload-config`.
fn push_settings(ctx: &mut DispatchCtx<'_>) {
    ctx.overlays
        .push(Box::new(crate::render::overlay::SettingsOverlay::open(
            phux_config::loader::config_path(),
            ctx.theme,
        )));
}

/// Capture a local bug-report bundle, copy its path (OSC 52), and toast it.
/// A write failure bells and toasts instead of panicking.
fn report_bug(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    effects: &mut ActionEffects,
) {
    let screen = focused.and_then(|id| {
        let terminal = published_terminal(ctx.engine_kernel, id)?;
        extract_selection_text(
            terminal,
            CopyRequest {
                start_row: 0,
                start_col: 0,
                end_row: 0,
                end_col: 0,
                mouse_anchor_screen: None,
                rectangle: false,
                cursor_row: 0,
                cursor_col: 0,
                grab: SelectionGrab::All,
            },
        )
    });
    let (alt_screen, mouse_tracking) = focused
        .and_then(|id| published_terminal(ctx.engine_kernel, id))
        .map(|terminal| {
            (
                terminal_in_alt_screen(terminal),
                terminal_wants_mouse_tracking(terminal),
            )
        })
        .unzip();
    let session = {
        let name = ctx.session_name.trim();
        (!name.is_empty()).then(|| name.to_owned())
    };
    let draft = crate::report::ReportDraft {
        note: str_arg(resolved, "note"),
        session,
        pane: focused.map(pane_selector),
        window: Some(ctx.workspace.active),
        viewport: Some(ctx.viewport),
        alt_screen,
        mouse_tracking,
        screen,
        version: env!("CARGO_PKG_VERSION").to_owned(),
    };
    match crate::report::write_bundle(&draft) {
        Ok(written) => {
            let path = written.dir.display().to_string();
            effects.clipboard = Some(path.clone());
            ctx.overlays.push(Box::new(ToastOverlay::passthrough(
                "bug report",
                vec![
                    format!("saved {}", written.id),
                    path,
                    "copied — phux report show".to_owned(),
                ],
                ctx.theme,
            )));
        }
        Err(err) => {
            tracing::warn!(error = %err, "report-bug: could not write bundle");
            effects.bell = true;
            ctx.overlays.push(Box::new(ToastOverlay::new(
                "bug report failed",
                vec![err.to_string()],
                ctx.theme,
            )));
        }
    }
}

fn pane_selector(id: &ResourceId) -> String {
    match id {
        ResourceId::Local { id } => format!("@{id}"),
        ResourceId::Satellite { host, id } => format!("{host}/@{id}"),
    }
}

/// Push the first-run onboarding hint card.
fn push_getting_started(ctx: &mut DispatchCtx<'_>) {
    ctx.overlays
        .push(Box::new(crate::render::overlay::ToastOverlay::passthrough(
            crate::attach::onboarding::ONBOARDING_TITLE,
            crate::attach::onboarding::hint_lines(ctx.keybindings, *ctx.sidebar_enabled),
            ctx.theme,
        )));
}

/// phux-wave-a-copy-mode: enter selection/copy mode. Arrow keys move
/// the cursor without extending the selection unless Shift is held;
/// mouse drag can select and copy in one gesture.
fn push_copy_mode(ctx: &mut DispatchCtx<'_>, focused: Option<&ResourceId>) {
    let pane_rect = focused_pane_rect(ctx, focused);
    let overlay = Box::new(crate::render::overlay::CopyModeOverlay::new(
        0,
        0,
        pane_rect.w,
        pane_rect.h,
    ));
    ctx.overlays.push(overlay);
}

/// ADR-0058: the keyboard route to the pane menu (the only one when the app
/// owns the mouse), anchored inside the focused pane's corner.
fn push_context_menu(ctx: &mut DispatchCtx<'_>, focused: Option<&ResourceId>) {
    let rect = focused_pane_rect(ctx, focused);
    let anchor = (rect.x.saturating_add(2), rect.y.saturating_add(1));
    let zoomed = ctx.zoomed.is_some();
    let spec = crate::attach::context_menu::pane_menu(ctx.keybindings, zoomed);
    open_context_menu(ctx, spec, anchor);
}

/// Push the grouped window picker (sessions as headers, one-step rows for
/// peers with cached layouts). Bells with no rows.
fn push_window_picker(ctx: &mut DispatchCtx<'_>, effects: &mut ActionEffects) {
    let items = window_picker_items(
        ctx.workspace,
        ctx.peers.sessions,
        ctx.peers.foreign_layouts,
        ctx.peers.focused_session,
    );
    if items.iter().all(SelectItem::is_header) {
        effects.bell = true;
        return;
    }
    ctx.overlays
        .push(Box::new(SelectList::new("Windows", items, ctx.theme)));
}

/// Push the session picker (grouped by host when a hub satellite or another
/// machine is known), with a "+ New session" row, and ask the driver for a
/// fresh host inventory.
fn push_session_picker(ctx: &mut DispatchCtx<'_>) {
    let items = session_picker_rows(&ctx.peers, ctx.workspace);
    *ctx.host_refresh_request = true;
    ctx.overlays.push(Box::new(
        SelectList::new("Sessions", items, ctx.theme).with_live_key(SESSION_PICKER_LIVE_KEY),
    ));
}

/// Push the live agent-fleet dashboard: every pane with its agent record,
/// attention, and branch; peers with cached layouts list one-step rows.
/// Bells with nothing to list.
fn push_agent_fleet(
    ctx: &mut DispatchCtx<'_>,
    panes: &HashMap<ResourceId, PaneSlot>,
    effects: &mut ActionEffects,
) {
    let meta = crate::attach::fleet::collect_pane_meta(
        panes,
        ctx.vcs,
        &crate::attach::agent_rows::agent_session_rows(ctx.engine_kernel),
    );
    let mut items = crate::attach::fleet::fleet_items(
        ctx.workspace,
        ctx.peers.sessions,
        ctx.peers.focused_session,
        ctx.agent_meta,
        &meta,
        ctx.peers.foreign_layouts,
        ctx.peers.foreign_agents,
    );
    items.extend(crate::attach::fleet::satellite_agent_items(
        ctx.peers.foreign_agents,
        ctx.peers.foreign_attention,
        ctx.workspace,
    ));
    if items.iter().all(SelectItem::is_header) {
        effects.bell = true;
        return;
    }
    ctx.overlays.push(Box::new(
        SelectList::new("Fleet", items, ctx.theme)
            .with_live_key(crate::attach::fleet::FLEET_LIVE_KEY),
    ));
}

/// ADR-0049: focus the next asking pane (window then DFS order, wrapping),
/// saving one return origin. No attention: bell, no origin.
fn next_attention(
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    panes: &HashMap<ResourceId, PaneSlot>,
    effects: &mut ActionEffects,
) {
    let ordered = ordered_workspace_panes(ctx.workspace);
    let current = focused.and_then(|id| ordered.iter().position(|(_, pane)| pane == id));
    let target = ordered
        .iter()
        .enumerate()
        .filter(|(_, (_, id))| panes.get(id).is_some_and(|slot| slot.attention))
        .find(|(index, _)| current.is_none_or(|current| *index > current))
        .or_else(|| {
            ordered
                .iter()
                .enumerate()
                .find(|(_, (_, id))| panes.get(id).is_some_and(|slot| slot.attention))
        })
        .map(|(_, (window, id))| (*window, id.clone()));
    let Some((window, target)) = target else {
        effects.bell = true;
        return;
    };

    ctx.attention_navigation.save_origin_once(focused);
    focus_terminal(ctx.workspace, window, target.clone());
    effects.layout_mutated = true;
    effects.set_focus = Some(target);
}

/// Jump back to the saved origin, consuming it even when that pane is gone
/// (a bell, never a sticky origin that could resolve elsewhere).
fn return_from_attention(ctx: &mut DispatchCtx<'_>, effects: &mut ActionEffects) {
    let Some(origin) = ctx.attention_navigation.take_origin() else {
        effects.bell = true;
        return;
    };
    let Some((window, _)) = ordered_workspace_panes(ctx.workspace)
        .into_iter()
        .find(|(_, id)| id == &origin)
    else {
        effects.bell = true;
        return;
    };
    focus_terminal(ctx.workspace, window, origin.clone());
    effects.layout_mutated = true;
    effects.set_focus = Some(origin);
}

/// Focus the pane at (window, DFS ordinal), per-client; stale coordinates
/// bell rather than focus the wrong pane.
fn focus_pane(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
) {
    let (Some(win), Some(ord)) = (usize_arg(resolved, "window"), usize_arg(resolved, "pane"))
    else {
        tracing::warn!(
            args = ?resolved.args,
            "focus-pane missing/bad `window`/`pane` args",
        );
        effects.bell = true;
        return;
    };
    let target = ctx
        .workspace
        .windows
        .get(win)
        .and_then(|w| w.state.tree.as_ref())
        .map(crate::layout::leaves)
        .and_then(|leaves| leaves.get(ord).cloned());
    let Some(target) = target else {
        tracing::warn!(
            window = win,
            pane = ord,
            "focus-pane: no such pane (layout changed?)",
        );
        effects.bell = true;
        return;
    };
    focus_in_window(ctx, effects, win, target);
}

/// Re-attach to another session. Optional `window`/`pane` make it a
/// one-step cross-session pick, `id`/`resource` pin identity, and `host`
/// takes the [`open_satellite_session`] path.
fn switch_session(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
) {
    let Some(name) = name_arg(resolved) else {
        tracing::warn!(
            args = ?resolved.args,
            "switch-session missing/bad `name` arg",
        );
        effects.bell = true;
        return;
    };
    if let Some(host) = str_arg(resolved, "host") {
        open_satellite_session(ctx, effects, &host, &name);
        return;
    }
    let window = usize_arg(resolved, "window");
    let pane = usize_arg(resolved, "pane");
    effects.reattach = Some(ReattachTarget::Existing {
        name,
        id: session_id_arg(resolved),
        window,
        pane,
        resource: resource_id_arg(resolved),
    });
}

/// ADR-0140: `switch-host { host, name }` — re-attach this terminal to
/// session `name` on another machine. `host` is a `phux.hosts/v1` row name:
/// a `[[remote]]` registry name, or `local` for this machine's own server.
/// The sidebar's machine segments and the session picker's machine groups
/// commit it.
fn switch_host(resolved: &phux_config::keybind::ResolvedAction, effects: &mut ActionEffects) {
    let (Some(host), Some(name)) = (str_arg(resolved, "host"), name_arg(resolved)) else {
        tracing::warn!(args = ?resolved.args, "switch-host needs `host` and `name` args");
        effects.bell = true;
        return;
    };
    effects.switch_host = Some((host, name));
}

/// phux-c2td.3: `switch-session { name, host }` — select a session that
/// lives on a satellite of this hub.
///
/// A session on another host cannot be *attached* from here: `ATTACH` is
/// session-scoped and session ids are not federation-routable (ADR-0016,
/// L1 §9.1), so the hub has no session of that name to re-attach this
/// client to. What the hub does relay is resources, so this reuses the
/// mechanism satellite panes already ride: the session's active pane,
/// re-tagged `Satellite { host, id }` by the hub's inventory, is opened as
/// a window of the session this client is attached to and attached through
/// the relay (`ATTACH_RESOURCE`), exactly as a `spawn --satellite` pane is.
/// Choosing the same session again focuses that window rather than opening
/// a second one onto the same pane.
///
/// The consequence to know: the opened window holds the satellite's real
/// Terminal, not a copy of it. Closing the window kills that pane on the
/// satellite, like any other leaf. A full cross-host attach — the
/// satellite's whole window layout, its own windows and splits — is
/// `phux attach --remote HOST SESSION`, a separate connection to that
/// server.
///
/// The window opens only when the attach succeeds. The action parks a
/// [`PendingWindow`] naming the pane to adopt and sends `ATTACH_RESOURCE`;
/// the reply either opens, focuses, and broadcasts the window, or — when the
/// hub or satellite refuses — bells and names the host and session in a
/// status notice, leaving the shared layout untouched. A second commit while
/// that attach is in flight sends nothing more.
///
/// Bells when the host is unreachable, has no such session, or reported no
/// active pane for it: there is nothing to open, and the picker's
/// `(unreachable)` header has already said why.
fn open_satellite_session(
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
    host: &str,
    name: &str,
) {
    let Some(target) = satellite_session_pane(ctx.peers.hosts, host, name) else {
        tracing::warn!(
            host,
            session = name,
            "switch-session: no reachable satellite session by that name",
        );
        effects.bell = true;
        return;
    };
    if let Some(index) = window_holding(ctx.workspace, &target) {
        focus_in_window(ctx, effects, index, target);
        return;
    }
    if attach_in_flight(ctx.pending_windows, &target) {
        return;
    }
    let request_id = ctx.take_request_id();
    ctx.pending_windows.insert(
        request_id,
        PendingWindow {
            name: format!("{host}/{name}"),
            adopt: Some(Adopt::Existing(target.clone())),
        },
    );
    // The pane exists already, so there is no spawn: attach it. Its
    // bootstrap seeds the slot, and the reply decides whether the window
    // opens (`server_frame::handler`).
    effects
        .command_frames
        .push(attach_frame(request_id, target));
}

/// Switch to window `index` and focus `target` in it (per-client).
fn focus_in_window(
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
    index: usize,
    target: ResourceId,
) {
    switch_window(ctx, effects, |workspace| {
        workspace.select(index);
    });
    if let Some(layout) = ctx.workspace.active_window_mut() {
        layout.focus = Some(target.clone());
    }
    effects.layout_mutated = true;
    effects.set_focus = Some(target);
}

/// Whether an attach adopting `target` into a window is already parked.
fn attach_in_flight(pending: &HashMap<u32, PendingWindow>, target: &ResourceId) -> bool {
    pending
        .values()
        .any(|window| window.adopt.as_ref().map(Adopt::pane) == Some(target))
}

/// The hub-routable pane behind a satellite session name, or `None` when
/// the host is absent, unreachable, or reported no active pane.
fn satellite_session_pane(
    hosts: &[phux_protocol::wire::info::HostInventory],
    host: &str,
    name: &str,
) -> Option<ResourceId> {
    hosts
        .iter()
        .find(|inventory| inventory.host.as_str() == host && inventory.is_reachable())?
        .sessions
        .iter()
        .find(|session| session.name == name)?
        .active_resource
        .clone()
}

/// The focused pane's host and directory, with an explicit `cwd` or `host`
/// argument overriding the matching field.
fn spawn_place(
    resolved: &phux_config::keybind::ResolvedAction,
    focused: Option<&ResourceId>,
    panes: &HashMap<ResourceId, PaneSlot>,
) -> phux_client_core::organization::Place {
    let focused_place = phux_client_core::organization::Place {
        host: focused
            .and_then(ResourceId::host)
            .map(|host| host.as_str().to_owned()),
        directory: focused
            .and_then(|id| panes.get(id))
            .and_then(|slot| slot.cwd.clone()),
    };
    let source = if str_arg(resolved, "cwd").is_none() && str_arg(resolved, "host").is_none() {
        phux_client_core::organization::PlaceSource::Focused
    } else {
        phux_client_core::organization::PlaceSource::Explicit {
            host: str_arg(resolved, "host"),
            directory: str_arg(resolved, "cwd"),
        }
    };
    phux_client_core::organization::place_for(Some(&focused_place), source)
}

fn remember_session(ctx: &mut DispatchCtx<'_>) {
    let name = ctx.session_name.clone();
    if name.is_empty() {
        return;
    }
    if ctx.session_mru.last().is_none_or(|last| last != &name) {
        ctx.session_mru.push(name);
    }
}

fn reattach_named(ctx: &mut DispatchCtx<'_>, effects: &mut ActionEffects, name: String) {
    remember_session(ctx);
    effects.reattach = Some(ReattachTarget::Existing {
        name,
        id: None,
        window: None,
        pane: None,
        resource: None,
    });
}

/// Switch to the session `step` away in name order (`1` next, `-1` previous).
fn step_session(ctx: &mut DispatchCtx<'_>, effects: &mut ActionEffects, step: i32) {
    let mut names: Vec<&str> = ctx
        .peers
        .sessions
        .iter()
        .map(|session| session.name.as_str())
        .collect();
    names.sort_unstable();
    let Some(name) = phux_client_core::organization::adjacent_name(&names, ctx.session_name, step)
    else {
        effects.bell = true;
        return;
    };
    reattach_named(ctx, effects, name.to_owned());
}

/// Switch to the session this client attached to before the current one.
fn last_session(ctx: &mut DispatchCtx<'_>, effects: &mut ActionEffects) {
    let history: Vec<&str> = ctx.session_mru.iter().map(String::as_str).collect();
    let Some(name) = phux_client_core::organization::last_name(&history, ctx.session_name) else {
        effects.bell = true;
        return;
    };
    let name = name.to_owned();
    reattach_named(ctx, effects, name);
}

/// Create-or-switch to a named session, or prompt for a name. A name no
/// selector could address again is refused with a notice, not created.
/// The seed directory and host follow the focused pane unless `cwd` or
/// `host` is set.
fn new_session(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    panes: &HashMap<ResourceId, PaneSlot>,
    effects: &mut ActionEffects,
) {
    let place = spawn_place(resolved, focused, panes);
    match name_arg(resolved) {
        Some(name) => match phux_client::rename::check_session_name(&name) {
            Ok(()) => {
                remember_session(ctx);
                effects.reattach = Some(ReattachTarget::Create {
                    name,
                    directory: place.directory,
                    host: place.host,
                });
            }
            Err(invalid) => {
                *ctx.rename_notice = Some(format!("could not create session {name}: {invalid}"));
            }
        },
        None => ctx
            .overlays
            .push(Box::new(PromptOverlay::new_session(ctx.theme))),
    }
}

/// Record a plugin action run; the async caller spawns it off the input loop.
fn plugin_action(resolved: &phux_config::keybind::ResolvedAction, effects: &mut ActionEffects) {
    let (Some(plugin), Some(action)) = (str_arg(resolved, "plugin"), str_arg(resolved, "action"))
    else {
        tracing::warn!(
            args = ?resolved.args,
            "plugin-action missing/bad `plugin`/`action` args",
        );
        effects.bell = true;
        return;
    };
    effects.run_plugin = Some((plugin, action));
}

/// Open a plugin `[[panes]]` entry as a Terminal via the ordinary spawn
/// (ADR-0017): `split`/`zoomed` park a split, `tab` a window, `overlay` a
/// floating box (ADR-0147). An unknown pair (disabled, typo) bells.
fn plugin_pane(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    effects: &mut ActionEffects,
) {
    let (Some(plugin), Some(pane)) = (str_arg(resolved, "plugin"), str_arg(resolved, "pane"))
    else {
        tracing::warn!(
            args = ?resolved.args,
            "plugin-pane missing/bad `plugin`/`pane` args",
        );
        effects.bell = true;
        return;
    };
    let Some(entry) = ctx
        .plugin_panes
        .iter()
        .find(|e| e.plugin_id == plugin && e.pane_id == pane)
    else {
        tracing::warn!(
            plugin = %plugin,
            pane = %pane,
            "plugin-pane names no hostable pane (unknown or disabled); dropping",
        );
        effects.bell = true;
        return;
    };
    let request_id = ctx.take_request_id();
    let mut frame = entry.spawn_frame(request_id);
    match entry.placement {
        HostedPlacement::Split | HostedPlacement::Zoomed => {
            let Some(focused_id) = focused.cloned() else {
                tracing::warn!(
                    plugin = %plugin,
                    pane = %pane,
                    "plugin-pane split/zoomed placement needs a focused pane; dropping",
                );
                effects.bell = true;
                return;
            };
            let pending = PendingSplit {
                focused_at_request: focused_id,
                // Side-by-side, matching the palette's
                // `split-pane` default (vertical divider).
                dir: SplitDir::Horizontal,
                zoom_on_spawn: entry.placement == HostedPlacement::Zoomed,
                host: SplitHost::Attached,
                adopt: None,
                open_existing: None,
            };
            set_spawn_initial_size(&mut frame, predicted_split_size(ctx, &pending));
            effects.spawn_terminal = Some((request_id, pending, frame));
        }
        HostedPlacement::Overlay => {
            // ADR-0147: sized to the box interior; no window adopts it.
            set_spawn_initial_size(
                &mut frame,
                spawn_initial_size(ctx, |content| {
                    let inner = crate::attach::floating::floating_box(content).inner;
                    Some((inner.w, inner.h))
                }),
            );
            effects.spawn_floating = Some((request_id, entry.title.clone(), frame));
        }
        HostedPlacement::Tab => {
            set_spawn_initial_size(
                &mut frame,
                spawn_initial_size(ctx, |content| Some((content.w, content.h))),
            );
            effects.spawn_window = Some((
                request_id,
                PendingWindow {
                    name: entry.title.clone(),
                    adopt: None,
                },
                frame,
            ));
        }
    }
}

/// Step the active window's focus with `step`, adopting the new state.
fn cycle_pane(
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
    step: impl FnOnce(&LayoutState) -> Option<LayoutState>,
) {
    if let Some(ls) = ctx.workspace.active_window_mut()
        && let Some(new_state) = step(ls)
    {
        let new_focus = new_state.focus.clone();
        *ls = new_state;
        effects.layout_mutated = true;
        effects.set_focus = new_focus;
    }
}

/// One-entry MRU jump-back, across windows; applying it records the pane
/// left, so repeats toggle.
fn last_pane(ctx: &mut DispatchCtx<'_>, focused: Option<&ResourceId>, effects: &mut ActionEffects) {
    let Some(target) = ctx.focus_history.target(focused, ctx.workspace) else {
        effects.bell = true;
        return;
    };
    let owner = ctx.workspace.windows.iter().position(|window| {
        window
            .state
            .tree
            .as_ref()
            .is_some_and(|tree| crate::layout::leaves(tree).contains(&target))
    });
    let Some(window) = owner else {
        tracing::debug!(terminal = ?target, "last-pane MRU target is no longer live");
        effects.bell = true;
        return;
    };
    ctx.workspace.active = window;
    ctx.workspace.windows[window].state.focus = Some(target.clone());
    effects.layout_mutated = true;
    effects.clear_predict = true;
    effects.set_focus = Some(target);
}

/// Zoom needs more than one pane (tmux bells); the same check permits
/// un-zooming, since the real tree keeps its leaves.
fn toggle_zoom(ctx: &DispatchCtx<'_>, effects: &mut ActionEffects) {
    let multi = ctx
        .workspace
        .active_window()
        .and_then(|ls| ls.tree.as_ref())
        .is_some_and(|t| crate::layout::leaves(t).len() > 1);
    if multi {
        effects.toggle_zoom = true;
        effects.layout_mutated = true;
    } else {
        effects.bell = true;
    }
}

/// Show/hide the sidebar. Showing it where it cannot fit would change
/// nothing on screen, so that bells instead; hiding is always allowed.
const fn toggle_sidebar(ctx: &DispatchCtx<'_>, effects: &mut ActionEffects) {
    if !*ctx.sidebar_enabled
        && crate::attach::paint::sidebar_reservation(
            ctx.viewport.0,
            true,
            *ctx.sidebar_width,
            crate::attach::paint::SidebarEdge::Left,
            ctx.chrome.min_pane_cols,
        )
        .is_none()
    {
        effects.bell = true;
        return;
    }
    // Show/hide the window sidebar. The driver owns
    // `sidebar_enabled`; we signal intent + a repaint so the panes
    // reflow into/out of the reserved columns.
    effects.toggle_sidebar = true;
    effects.layout_mutated = true;
}
