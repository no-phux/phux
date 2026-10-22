//! The action interpreter and its handlers. [`run_action`] looks the
//! action up in the registry ([`crate::attach::action_registry::ACTIONS`])
//! and calls the handler its row names; each handler takes one
//! [`ActionCall`] (ADR-0172). The handlers live in submodules by what they
//! act on and are re-exported here, where the registry names them.

use std::collections::HashMap;

use phux_protocol::ResourceId;
use phux_protocol::wire::frame::{Command, FrameKind};

use crate::attach::actions::{Adopt, PendingWindow};
use crate::attach::pane_state::PaneSlot;

use super::args::str_arg;
use super::ctx::DispatchCtx;
use super::effects::ActionEffects;
use super::pickers::switch_window;

mod modes;
mod panes;
mod plugins;
mod sessions;
mod spawn;
mod views;
mod windows;

pub(in crate::attach) use modes::{enter_mode, exit_mode};
pub(in crate::attach) use panes::{
    focus_direction, give_input, kill_focused_pane, last_pane, move_pane, next_pane, previous_pane,
    resize_pane, set_pane, signal_terminal, split_pane, take_input, toggle_sidebar, toggle_zoom,
};
pub(in crate::attach) use plugins::{plugin_action, plugin_pane};
pub(in crate::attach) use sessions::{
    detach, last_session, new_session, next_session, previous_session, rename_session, switch_host,
    switch_session,
};
pub(in crate::attach) use spawn::{find_path, go_to_directory, insert_path, new_window};
pub(in crate::attach) use views::{
    agents_filter, focus_pane, next_attention, push_action_finder, push_agent_fleet,
    push_context_menu, push_copy_mode, push_getting_started, push_session_picker, push_settings,
    push_window_picker, rec_mark, reload_config, report_bug, return_from_attention,
    toggle_project_group,
};
#[cfg(test)]
pub(super) use windows::kill_window_confirm;
pub(in crate::attach) use windows::{
    close_view, kill_window, move_window, next_window, previous_window, rename_window,
    reopen_views, select_window,
};

/// Everything a handler may read or change for one action: the action, the
/// lent driver state, the focused pane, the read-only pane slots (lent apart
/// from `ctx` because the driver lends `panes` mutably beside it), and the
/// effects the async pass applies.
pub(in crate::attach) struct ActionCall<'a, 'c> {
    pub(super) resolved: &'a phux_config::keybind::ResolvedAction,
    pub(super) ctx: &'a mut DispatchCtx<'c>,
    pub(super) focused: Option<&'a ResourceId>,
    pub(super) panes: &'a HashMap<ResourceId, PaneSlot>,
    pub(super) effects: &'a mut ActionEffects,
}

/// A registry row's handler.
pub(in crate::attach) type ActionHandler = fn(ActionCall<'_, '_>);

/// Dispatch a resolved action through the handler its registry row names.
/// Sync by design; the caller does the frame I/O, so a send failure never
/// leaves layout state half-mutated.
pub(super) fn run_action(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    panes: &HashMap<ResourceId, PaneSlot>,
) -> ActionEffects {
    tracing::info!(action = %resolved.action, "input: running resolved action");
    let mut effects = ActionEffects::default();
    let Some(spec) = crate::attach::action_registry::find(&resolved.action) else {
        tracing::debug!(action = %resolved.action, "unhandled resolved action");
        return effects;
    };
    (spec.run)(ActionCall {
        resolved,
        ctx,
        focused,
        panes,
        effects: &mut effects,
    });
    effects
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

/// `ATTACH_RESOURCE` for an existing pane.
fn attach_frame(request_id: u32, terminal_id: ResourceId) -> FrameKind {
    FrameKind::Command {
        request_id,
        command: Command::attach_terminal(
            terminal_id,
            crate::attach::attach_role::pane_attach_role(),
        ),
    }
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
    if let Some(layout) = ctx.workspace.active_group_mut() {
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
