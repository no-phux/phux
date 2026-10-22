//! Pane handlers: splitting, moving, killing, focusing, resizing, zooming,
//! and the pane's input and signals.

use phux_client::layout_ops::DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID;
use phux_protocol::ResourceId;
use phux_protocol::ids::SatelliteHost;
use phux_protocol::wire::frame::{Command, FrameKind, InputMode};

use crate::attach::actions::{self, ActionError, PendingSplit, SplitHost};
use crate::attach::directory_picker::DirectorySupport;
use crate::attach::input_dispatch::args::{
    PaneMouseArg, amount_arg, direction_arg, kill_resource_frame, mouse_arg, resource_id_arg,
    signal_arg, split_dir_arg,
};
use crate::attach::input_dispatch::ctx::DispatchCtx;
use crate::attach::input_dispatch::dispatch::predicted_split_size;
use crate::attach::input_dispatch::effects::{ActionEffects, PaneMoveIntent};
use crate::attach::input_dispatch::pickers::{move_pane_picker_items, window_holding};
use crate::layout::{LayoutState, SplitDir};
use crate::render::overlay::SelectList;

use super::{
    ActionCall, attach_frame, attach_in_flight, bind_satellite_spawn, focus_in_window, spawn_place,
};

/// `next-pane`: cycle focus forward through the active tab.
pub(in crate::attach) fn next_pane(ActionCall { ctx, effects, .. }: ActionCall<'_, '_>) {
    cycle_pane(ctx, effects, actions::apply_next_pane);
}

/// `previous-pane`: cycle focus backward through the active tab.
pub(in crate::attach) fn previous_pane(ActionCall { ctx, effects, .. }: ActionCall<'_, '_>) {
    cycle_pane(ctx, effects, actions::apply_previous_pane);
}

/// Open the all-session destination picker, or commit its exact selected row.
/// Side-by-side at 0.5 is the single keyboard-fast placement policy.
pub(in crate::attach) fn move_pane(
    ActionCall {
        resolved,
        ctx,
        focused,
        effects,
        ..
    }: ActionCall<'_, '_>,
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
pub(in crate::attach) fn split_pane(
    ActionCall {
        resolved,
        ctx,
        focused,
        panes,
        effects,
    }: ActionCall<'_, '_>,
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
pub(in crate::attach) fn kill_focused_pane(
    ActionCall {
        ctx,
        focused,
        effects,
        ..
    }: ActionCall<'_, '_>,
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
pub(in crate::attach) fn take_input(
    ActionCall {
        ctx,
        focused,
        effects,
        ..
    }: ActionCall<'_, '_>,
) {
    focused_command(ctx, focused, effects, |terminal_id| Command::AcquireInput {
        terminal_id,
        mode: InputMode::Seize,
        ttl_ms: 0,
    });
}

/// ADR-0033: release the focused pane's input lease.
pub(in crate::attach) fn give_input(
    ActionCall {
        ctx,
        focused,
        effects,
        ..
    }: ActionCall<'_, '_>,
) {
    focused_command(ctx, focused, effects, |terminal_id| Command::ReleaseInput {
        terminal_id,
    });
}

/// ADR-0033: deliver a POSIX signal to the focused pane's process
/// group. `freeze`/`resume` is the reversible brake; distinct from
/// `kill-pane`, which removes the pane.
pub(in crate::attach) fn signal_terminal(
    ActionCall {
        resolved,
        ctx,
        focused,
        effects,
        ..
    }: ActionCall<'_, '_>,
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
pub(in crate::attach) fn set_pane(
    ActionCall {
        resolved,
        ctx,
        focused,
        effects,
        ..
    }: ActionCall<'_, '_>,
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

/// Move focus to the neighbouring pane in the requested direction.
pub(in crate::attach) fn focus_direction(
    ActionCall {
        resolved,
        ctx,
        effects,
        ..
    }: ActionCall<'_, '_>,
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
pub(in crate::attach) fn resize_pane(
    ActionCall {
        resolved,
        ctx,
        effects,
        ..
    }: ActionCall<'_, '_>,
) {
    let (Some(dir), Some(amount)) = (direction_arg(resolved), amount_arg(resolved)) else {
        tracing::warn!(args = ?resolved.args, "resize-pane missing args");
        effects.bell = true;
        return;
    };
    let Some(ls) = ctx.workspace.active_group_mut() else {
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

/// Step the active window's focus with `step`, adopting the new state.
fn cycle_pane(
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
    step: impl FnOnce(&LayoutState) -> Option<LayoutState>,
) {
    if let Some(ls) = ctx.workspace.active_group_mut()
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
pub(in crate::attach) fn last_pane(
    ActionCall {
        ctx,
        focused,
        effects,
        ..
    }: ActionCall<'_, '_>,
) {
    let Some(target) = ctx.focus_history.target(focused, ctx.workspace) else {
        effects.bell = true;
        return;
    };
    let owner = ctx.workspace.groups.iter().position(|window| {
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
    ctx.workspace.groups[window].state.focus = Some(target.clone());
    effects.layout_mutated = true;
    effects.clear_predict = true;
    effects.set_focus = Some(target);
}

/// Zoom needs more than one pane (tmux bells); the same check permits
/// un-zooming, since the real tree keeps its leaves.
pub(in crate::attach) fn toggle_zoom(ActionCall { ctx, effects, .. }: ActionCall<'_, '_>) {
    let multi = ctx
        .workspace
        .active_group()
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
pub(in crate::attach) const fn toggle_sidebar(ActionCall { ctx, effects, .. }: ActionCall<'_, '_>) {
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
