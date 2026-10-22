//! Session handlers: cycling, switching, creating, renaming, detaching, and
//! satellite sessions.

use phux_protocol::ResourceId;

use crate::attach::actions::{Adopt, PendingWindow};
use crate::attach::input_dispatch::args::{
    name_arg, resource_id_arg, session_id_arg, str_arg, usize_arg,
};
use crate::attach::input_dispatch::ctx::DispatchCtx;
use crate::attach::input_dispatch::effects::{ActionEffects, ReattachTarget};
use crate::attach::input_dispatch::pickers::window_holding;
use crate::render::overlay::PromptOverlay;

use super::{ActionCall, attach_frame, attach_in_flight, focus_in_window, spawn_place};

/// `next-session`: the session after this one by name.
pub(in crate::attach) fn next_session(ActionCall { ctx, effects, .. }: ActionCall<'_, '_>) {
    step_session(ctx, effects, 1);
}

/// `previous-session`: the session before this one by name.
pub(in crate::attach) fn previous_session(ActionCall { ctx, effects, .. }: ActionCall<'_, '_>) {
    step_session(ctx, effects, -1);
}

/// `detach`: the effect pass sends `DETACH`.
pub(in crate::attach) const fn detach(ActionCall { effects, .. }: ActionCall<'_, '_>) {
    effects.detach = true;
}

/// Rename the attached session (`name`), or prompt for a name that commits
/// back through here. The send happens in `apply_action_effects`.
pub(in crate::attach) fn rename_session(
    ActionCall {
        resolved,
        ctx,
        effects,
        ..
    }: ActionCall<'_, '_>,
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

/// Re-attach to another session. Optional `window`/`pane` make it a
/// one-step cross-session pick, `id`/`resource` pin identity, and `host`
/// takes the [`open_satellite_session`] path.
pub(in crate::attach) fn switch_session(
    ActionCall {
        resolved,
        ctx,
        effects,
        ..
    }: ActionCall<'_, '_>,
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
pub(in crate::attach) fn switch_host(
    ActionCall {
        resolved, effects, ..
    }: ActionCall<'_, '_>,
) {
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

fn remember_session(ctx: &mut DispatchCtx<'_>) {
    let name = ctx.session_name.clone();
    ctx.focus_history.remember_session(&name);
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
pub(in crate::attach) fn last_session(ActionCall { ctx, effects, .. }: ActionCall<'_, '_>) {
    let history: Vec<&str> = ctx
        .focus_history
        .session_names()
        .iter()
        .map(String::as_str)
        .collect();
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
pub(in crate::attach) fn new_session(
    ActionCall {
        resolved,
        ctx,
        focused,
        panes,
        effects,
    }: ActionCall<'_, '_>,
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
