//! The action interpreter: one arm per canonical action name, plus the
//! action-finder overlay push.

//! Input dispatcher: translates parser-emitted events into wire frames
//! or layout-action effects.
//!
//! Owns the resolver-intercept path (prefix chord → `ResolvedAction` →
//! mutate the active window of the `Workspace`), the predict overlay's
//! keystroke feed, and the parked-spawn bookkeeping (`PendingSplit` /
//! `PendingWindow`) that bridges a local `split-pane` / `new-window`
//! chord to its remote `SPAWN_RESOURCE` reply.

use std::collections::HashMap;

use phux_protocol::ResourceId;
use phux_protocol::ids::SatelliteHost;
use phux_protocol::wire::frame::{Command, FrameKind, InputMode};

use crate::attach::actions::{self, ActionError, Adopt, PendingSplit, PendingWindow, SplitHost};
use crate::attach::directory_picker::{DirectorySupport, ListingHost, PendingDirectory};
use crate::attach::pane_state::PaneSlot;
use crate::attach::plugin_panes::HostedPlacement;
use crate::layout::{LayoutState, SplitDir, Workspace};
use crate::render::overlay::{PendingOverlay, PromptOverlay, SelectItem, SelectList};
use phux_client::layout_ops::DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID;

use super::args::{
    PaneMouseArg, amount_arg, direction_arg, focus_terminal, index_arg, mouse_arg, name_arg,
    ordered_workspace_panes, signal_arg, soft_kill_input_frames, split_dir_arg, str_arg, usize_arg,
};
use super::ctx::DispatchCtx;
use super::dispatch::{
    focused_pane_rect, open_context_menu, predicted_split_size, set_spawn_initial_size,
    spawn_initial_size,
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
    ctx.overlays.push(Box::new(SelectList::new(
        "Commands & Help",
        items,
        ctx.theme,
    )));
}

/// Take the next client request id, advancing the driver's counter.
const fn take_request_id(ctx: &mut DispatchCtx<'_>) -> u32 {
    let request_id = *ctx.next_request_id;
    *ctx.next_request_id = ctx.next_request_id.wrapping_add(1);
    request_id
}

/// Dispatch a resolved action against the driver's context.
///
/// Returns the [`ActionEffects`] the caller needs to apply. The function
/// is sync: it never touches the connection — frame I/O happens in the
/// caller (`dispatch_input_events`) so a hypothetical async wire-send
/// failure doesn't leave layout state half-mutated.
///
/// The body is the central dispatch table: one line per canonical action
/// name, delegating to the private per-action helper below it.
pub(super) fn run_action(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    // phux-foz.7: read-only view of the live pane slots. The `agent-fleet`
    // arm snapshots each pane's asked flag / OSC title / cwd from it;
    // every other arm ignores it. Threaded as a parameter (not a ctx
    // field) because the driver also passes `panes` mutably alongside the
    // ctx into `dispatch_input_events`.
    panes: &HashMap<ResourceId, PaneSlot>,
) -> ActionEffects {
    // One event per resolved action the user triggered. Info level: a
    // keybinding firing is a user-lifecycle event a trace reader wants under
    // the default filter, and it is human-paced (not per-frame), so it costs
    // nothing meaningful on the hot path. The action name is the key field;
    // any render-triggering effect is captured by the resulting repaint /
    // frame spans downstream.
    tracing::info!(action = %resolved.action, "input: running resolved action");
    let mut effects = ActionEffects::default();
    let e = &mut effects;
    match resolved.action.as_str() {
        "split-pane" => split_pane(resolved, ctx, focused, panes, e),
        "move-pane" => move_pane(resolved, ctx, focused, e),
        "kill-pane" => kill_focused_pane(focused, e),
        "take-input" => take_input(ctx, focused, e),
        "give-input" => give_input(ctx, focused, e),
        "signal-terminal" => signal_terminal(resolved, ctx, focused, e),
        "set-pane" => set_pane(resolved, ctx, focused, e),
        "new-window" => new_window(resolved, ctx, e),
        "go-to-directory" => go_to_directory(resolved, ctx, focused, panes, e),
        "kill-window" => kill_active_window(ctx, e),
        "next-window" => switch_window(ctx, e, Workspace::next),
        "previous-window" => switch_window(ctx, e, Workspace::prev),
        "select-window" => select_window(resolved, ctx, e),
        "rename-window" => rename_window(resolved, ctx, e),
        "rename-session" => rename_session(resolved, ctx, e),
        "focus-direction" => focus_direction(resolved, ctx, e),
        "resize-pane" => resize_pane(resolved, ctx, e),
        "reload-config" => reload_config(e),
        "show-help" | "command-palette" => push_action_finder(ctx),
        "getting-started" => push_getting_started(ctx),
        "settings" => push_settings(ctx),
        "copy-mode" => push_copy_mode(ctx, focused),
        "context-menu" => push_context_menu(ctx, focused),
        "window-picker" => push_window_picker(ctx, e),
        "session-picker" => push_session_picker(ctx),
        "agent-fleet" => push_agent_fleet(ctx, panes, e),
        "next-attention" => next_attention(ctx, focused, panes, e),
        "return-from-attention" => return_from_attention(ctx, e),
        "focus-pane" => focus_pane(resolved, ctx, e),
        "switch-session" => switch_session(resolved, ctx, e),
        "new-session" => new_session(resolved, ctx, e),
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
            ctx.focused_session,
            ctx.sessions,
            ctx.foreign_layouts,
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

/// phux-4li.12: `SPAWN_RESOURCE` → server allocates the new
/// Terminal under `DEFAULT_GROUP_ID` and replies with
/// `RESOURCE_SPAWNED { request_id, result: Ok(new_id) }`. The
/// layout mutation happens in the reply handler — see
/// `handle_server_frame`'s `ResourceSpawned` arm and
/// `apply_spawned_ok`. We park a `PendingSplit` keyed by
/// request id so the reply knows which leaf to split.
///
/// phux-c2td.18: splitting a satellite pane spawns the new pane on that
/// satellite through the attached hub (`SPAWN_RESOURCE.satellite`), at the
/// focused pane's directory there when the client knows it. The reply
/// attaches the relayed pane and the split applies only when that attach
/// succeeds (`server_frame::handler`). A hub without host-aware spawns
/// ([`split_host`]) spawns the pane on itself, as before, and the reply's
/// notice says so. A local split is unchanged: no host, no cwd.
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
    let request_id = take_request_id(ctx);
    let host = split_host(&focused_id, ctx.directory_support);
    let satellite = match &host {
        SplitHost::Satellite(satellite) => Some(satellite.clone()),
        SplitHost::Attached | SplitHost::AttachedInsteadOf(_) => None,
    };
    // A local split's cwd inheritance is phux-4li.1; until then the
    // server picks (typically $HOME). A satellite split starts where the
    // pane it splits is, on that satellite. `command = None` invokes the
    // spawning server's default shell; `env = None` inherits its
    // environment as-is.
    let cwd = satellite
        .as_ref()
        .and_then(|satellite| pane_cwd_on(Some(satellite), Some(&focused_id), panes));
    let pending = PendingSplit {
        focused_at_request: focused_id,
        dir,
        zoom_on_spawn: false,
        host,
        adopt: None,
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

/// phux-c2td.25: ask a satellite spawn to bind its pane to the satellite's
/// instance token (ADR-0109), so a pane the spawn strands can later be
/// killed conditionally. A hub or satellite without `CONDITIONAL_KILL`
/// skips the field by length and answers unbound; a local spawn is left
/// byte-identical.
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

/// The host a split of `focused` spawns on (phux-c2td.18).
///
/// A local pane splits on the attached server. A satellite pane splits on
/// its satellite when the hub advertises `LIST_DIRECTORY_HOST`, the bit that
/// shipped with host-aware spawns (`new-window { host }`). A hub without it
/// may predate `SPAWN_RESOURCE.satellite`, and such a peer skips the
/// unknown field and spawns on itself, so against it the split stays on the
/// hub and says so rather than landing somewhere the user did not expect.
fn split_host(focused: &ResourceId, support: DirectorySupport) -> SplitHost {
    let Some(host) = focused.host() else {
        return SplitHost::Attached;
    };
    if support == DirectorySupport::HostAware {
        SplitHost::Satellite(host.clone())
    } else {
        SplitHost::AttachedInsteadOf(host.clone())
    }
}

/// phux-4li.12: soft-kill — write `exit\n` as a sequence of
/// `INPUT_KEY` events to the focused Terminal. When the shell
/// processes those keystrokes it exits, the PTY closes, and
/// the server broadcasts `RESOURCE_CLOSED` which we then fold
/// out of the layout in `handle_server_frame`.
///
/// Caveat: this is softer than tmux's `kill-pane`, which
/// sends SIGKILL to the entire process group. If the
/// focused pane has an unresponsive foreground process
/// (e.g. a stuck `cat` blocked on a non-existent FIFO) the
/// keystrokes go nowhere. A future ticket may add an
/// explicit `KILL_RESOURCE` wire frame; for v0.1 this gets
/// the daily-drive flow working end-to-end.
fn kill_focused_pane(focused: Option<&ResourceId>, effects: &mut ActionEffects) {
    let Some(focused_id) = focused.cloned() else {
        tracing::warn!("kill-pane: no focused pane to kill; dropping action");
        effects.bell = true;
        return;
    };
    effects.kill_frames = soft_kill_input_frames(&focused_id);
    // phux-i0e8.2.2: mark the close as ours so the resulting
    // RESOURCE_CLOSED does not raise a pane-exit notice.
    effects.expected_closes = vec![focused_id];
}

/// ADR-0033: seize the focused pane's input lease so only this
/// client's keystrokes reach the PTY. `Seize` preempts any holder;
/// the server broadcasts `TerminalControl` so the badge updates.
fn take_input(
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    effects: &mut ActionEffects,
) {
    let Some(focused_id) = focused.cloned() else {
        tracing::warn!("take-input: no focused pane; dropping action");
        effects.bell = true;
        return;
    };
    let request_id = take_request_id(ctx);
    effects.command_frames.push(FrameKind::Command {
        request_id,
        command: Command::AcquireInput {
            terminal_id: focused_id,
            mode: InputMode::Seize,
            ttl_ms: 0,
        },
    });
}

/// ADR-0033: release the focused pane's input lease back to open
/// input. A no-op server-side if we do not hold it.
fn give_input(
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&ResourceId>,
    effects: &mut ActionEffects,
) {
    let Some(focused_id) = focused.cloned() else {
        tracing::warn!("give-input: no focused pane; dropping action");
        effects.bell = true;
        return;
    };
    let request_id = take_request_id(ctx);
    effects.command_frames.push(FrameKind::Command {
        request_id,
        command: Command::ReleaseInput {
            terminal_id: focused_id,
        },
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
    let Some(focused_id) = focused.cloned() else {
        tracing::warn!("signal-terminal: no focused pane; dropping action");
        effects.bell = true;
        return;
    };
    let request_id = take_request_id(ctx);
    effects.command_frames.push(FrameKind::Command {
        request_id,
        command: Command::SignalTerminal {
            terminal_id: focused_id,
            signal,
        },
    });
}

/// phux-npb3 (ADR-0048 decision 3 follow-up): flip the focused
/// pane's per-pane mouse opt-out. `mouse = "off"` opts the pane
/// out of client mouse handling (no synthesized `INPUT_MOUSE`; the
/// driver drops outer capture while the pane is focused, so the
/// host terminal's raw mouse handling returns for it alone);
/// `"on"` opts back in; `"toggle"` flips. Entirely client-local —
/// nothing crosses the wire.
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

/// phux-4li.15: open a new window. Spawn a fresh Terminal
/// (same SPAWN as a split) and park a `PendingWindow`; the
/// reply (`handle_server_frame`'s `ResourceSpawned` arm) adds a
/// window seeded on the spawned pane and makes it active. The
/// new pane is a bare leaf — the server files it under the
/// default Group; the TUI groups it into a window itself
/// (windows are a client convention, ADR-0017).
///
/// An optional `cwd` arg starts the window's shell there. The path is
/// interpreted by the attached server, on its own host — the confirm row of
/// the `go-to-directory` picker commits exactly this. An optional `host` arg
/// spawns the window on that satellite through the attached hub
/// (`SPAWN_RESOURCE.satellite`), `cwd` then naming a path on the satellite:
/// the confirm row of a satellite listing. The reply attaches the relayed
/// pane (`server_frame::handler::handle_window_spawned`).
fn new_window(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
) {
    let request_id = take_request_id(ctx);
    let name = ctx.workspace.default_window_name();
    let mut frame = FrameKind::SpawnResource {
        request_id,
        group: DEFAULT_GROUP_ID,
        command: None,
        cwd: str_arg(resolved, "cwd"),
        env: None,
        term: None,
        satellite: host_arg(resolved),
        owner_terminal: None,
        agent_session: None,
        // phux-a5xj: the new window holds one leaf, so the pane
        // fills the whole content rect. Predicting that here spares
        // the pane a bootstrap-then-reflow round trip.
        initial_size: spawn_initial_size(ctx, |content| Some((content.w, content.h))),
        resource: None,
    };
    bind_satellite_spawn(&mut frame);
    effects.spawn_window = Some((request_id, PendingWindow { name, adopt: None }, frame));
}

/// Browse directories on the host a listing reads (`docs/spec/L3.md` §4).
///
/// The host is the `host` arg, else the focused pane's satellite, else the
/// attached server ([`listing_host`]). The listing starts at the `path` arg,
/// else the focused pane's directory when that pane lives on the listed
/// host, else that host user's home (the empty path); the reply opens the
/// picker (`crate::attach::directory_picker`). Against a server that does
/// not advertise the query the action bells and sends nothing.
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
    let request_id = take_request_id(ctx);
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

/// A non-empty `host` arg as a satellite name.
fn host_arg(resolved: &phux_config::keybind::ResolvedAction) -> Option<SatelliteHost> {
    str_arg(resolved, "host")
        .filter(|host| !host.is_empty())
        .map(SatelliteHost::new)
}

/// The host one listing reads: `wanted` (the `host` arg), else the focused
/// pane's satellite, else the attached server. A hub that predates
/// `LIST_DIRECTORY.host` would skip the field and list itself, so against
/// one the request stays on the attached server and the picker says so.
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

/// phux-4li.15: soft-kill every pane in the active window, the
/// same `exit\n` mechanism as `kill-pane`. As each
/// `RESOURCE_CLOSED` lands, `handle_server_frame` folds the pane
/// out; when the window's tree empties it is pruned and the
/// new layout broadcast. No synchronous window removal here.
fn kill_active_window(ctx: &DispatchCtx<'_>, effects: &mut ActionEffects) {
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
    effects.kill_frames = leaves.iter().flat_map(soft_kill_input_frames).collect();
    // phux-i0e8.2.2: every pane in the window dies at our request;
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
        // `rename-window` with the typed name (phux-ahv.1).
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

/// Rename the session this client is attached to. With an explicit
/// `name` it renames directly; with no name it opens a prompt
/// pre-filled with the current session name, which commits
/// `rename-session { name }` back through this same path (the
/// rename-window precedent). The actual `RENAME_SESSION` send +
/// optimistic local-name update happen in `apply_action_effects`
/// (the connection is async, `run_action` is sync — the `detach`
/// model).
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
    if let Some(ls) = ctx.workspace.active_window_mut()
        && let Some(new_state) = actions::apply_focus(ls, dir)
    {
        let new_focus = new_state.focus.clone();
        *ls = new_state;
        effects.layout_mutated = true;
        effects.set_focus = new_focus;
    }
    // No-neighbour case: silently drop (tmux convention —
    // bumping into the layout edge isn't a bell).
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

/// phux-foz.5: explicit live config reload. The actual re-read +
/// swap happens in the driver after this batch (see
/// `DispatchCtx::reload_request`): the resolver that just
/// resolved this chord, the theme, and the keybindings
/// snapshot are all borrowed by `ctx` right now — they are
/// exactly the state the reload replaces.
const fn reload_config(effects: &mut ActionEffects) {
    effects.reload_config = true;
}

/// ADR-0101: open the settings page over the canonical config file. The
/// page reads the file itself and writes it one key at a time; a saved
/// edit comes back as `OverlayOutcome::ReloadConfig`, which the dispatcher
/// hands up as a `reload-config`.
fn push_settings(ctx: &mut DispatchCtx<'_>) {
    ctx.overlays
        .push(Box::new(crate::render::overlay::SettingsOverlay::open(
            phux_config::loader::config_path(),
            ctx.theme,
        )));
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

/// phux-wrnm (ADR-0058): the keyboard route to the pane menu, and
/// the only route for a pane whose app owns the mouse. Anchored
/// just inside the focused pane's top-left corner so it opens over
/// the pane it acts on, wherever that pane sits in the layout.
fn push_context_menu(ctx: &mut DispatchCtx<'_>, focused: Option<&ResourceId>) {
    let rect = focused_pane_rect(ctx, focused);
    let anchor = (rect.x.saturating_add(2), rect.y.saturating_add(1));
    let zoomed = ctx.zoomed.is_some();
    let spec = crate::attach::context_menu::pane_menu(ctx.keybindings, zoomed);
    open_context_menu(ctx, spec, anchor);
}

/// phux-4li.19 / nav: push the `<leader> w` grouped window
/// picker. Sessions are section headers; under the current
/// session each window (`index:name`, pane count) commits
/// `select-window { index }` (the same per-client switch the
/// numeric prefix bindings use). Other sessions list their own
/// windows as one-step `switch-session { name, window }` rows
/// when their persisted layout is cached (phux-foz.8), falling
/// back to a single "switch to session" row otherwise. With no
/// rows at all it bells.
fn push_window_picker(ctx: &mut DispatchCtx<'_>, effects: &mut ActionEffects) {
    let items = window_picker_items(
        ctx.workspace,
        ctx.sessions,
        ctx.foreign_layouts,
        ctx.focused_session,
    );
    if items.iter().all(SelectItem::is_header) {
        effects.bell = true;
        return;
    }
    ctx.overlays
        .push(Box::new(SelectList::new("Windows", items, ctx.theme)));
}

/// phux-4li.20: push the session picker. The current session is
/// first and marked in its secondary text so the list is a full
/// inventory and opens with useful orientation. Committing that
/// row dismisses the picker as a silent no-op; peer rows commit
/// `switch-session { name }`. A trailing "+ New session" row
/// keeps creation reachable even when no sessions are cached.
///
/// phux-c2td.3: against a federation hub the rows are grouped by
/// host — this host, then each satellite — and a satellite row
/// commits `switch-session { name, host }`. The open also asks
/// the driver for a fresh inventory; the list carries the live
/// key so that reply refreshes these rows in place.
fn push_session_picker(ctx: &mut DispatchCtx<'_>) {
    let items = session_picker_rows(ctx.sessions, ctx.focused_session, ctx.hosts, ctx.workspace);
    *ctx.host_refresh_request = true;
    ctx.overlays.push(Box::new(
        SelectList::new("Sessions & hosts", items, ctx.theme)
            .with_live_key(SESSION_PICKER_LIVE_KEY),
    ));
}

/// phux-foz.7: push the agent-fleet dashboard — every pane of
/// the attached session grouped under session headers, with its
/// ADR-0040 agent record (name/kind + state glyph), ADR-0035
/// asked/attention highlight, and branch/cwd. Current-session
/// rows commit `focus-pane { window, pane }` through the single
/// dispatch path.
///
/// phux-jpqd: a FOREIGN session with a cached persisted layout
/// (`foreign_layouts`) lists one row per pane committing a
/// one-step `switch-session { name, window, pane }`, its agent
/// glyph/state drawn from `foreign_agents` — no attach hop to see
/// a peer's panes. A foreign session with no cached layout still
/// falls back to a single `switch-session { name }` row.
/// Constructed with the fleet live key so the driver refreshes
/// the rows in place as agent events land while it is open. With
/// nothing to list it bells.
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
    let items = crate::attach::fleet::fleet_items(
        ctx.workspace,
        ctx.sessions,
        ctx.focused_session,
        ctx.agent_meta,
        &meta,
        ctx.foreign_layouts,
        ctx.foreign_agents,
    );
    if items.iter().all(SelectItem::is_header) {
        effects.bell = true;
        return;
    }
    ctx.overlays.push(Box::new(
        SelectList::new("Agent fleet", items, ctx.theme)
            .with_live_key(crate::attach::fleet::FLEET_LIVE_KEY),
    ));
}

/// phux-oih5.16 / ADR-0049: advisory, client-local navigation over
/// asking panes. Flatten windows in display order and each tree in
/// DFS leaf order; choose the first asking pane strictly after the
/// current pane, wrapping once. No attention means a bell-no-op and
/// does not arm a return origin.
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

/// Jump back to the pane `next-attention` first navigated away from.
///
/// Consume first: a pane that disappeared while we were cycling is
/// a safe bell-no-op, not a sticky origin that can later resolve to
/// a different pane. `ResourceId` is stable across window reordering,
/// so a surviving origin is found in its current window/DFS slot.
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

/// phux-foz.7: focus a specific pane addressed as
/// (window index, DFS leaf ordinal) — the commit the fleet
/// dashboard's current-session rows carry. Per-client, like
/// `select-window` (no broadcast): switch to the window, then
/// move its client-local focus onto the target leaf. Stale
/// coordinates (the layout changed since the rows were built)
/// bell rather than focusing the wrong pane.
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
    switch_window(ctx, effects, |w| {
        w.select(win);
    });
    if let Some(ls) = ctx.workspace.active_window_mut() {
        ls.focus = Some(target.clone());
    }
    effects.layout_mutated = true;
    effects.set_focus = Some(target);
}

/// phux-4li.20 / phux-eb0: re-target this client to another
/// session. The effect carries the target up to
/// `apply_action_effects`, which routes it to the driver's
/// outer re-attach loop (in-process re-attach on the same
/// connection). A bad/absent `name` arg bells.
///
/// phux-foz.8: an optional `window = N` arg makes it the
/// one-step cross-session window pick — after the re-attach
/// loads the target's persisted layout, the driver selects
/// window `N`. The grouped window picker's foreign-session
/// rows commit this form.
///
/// phux-jpqd: an additional optional `pane = P` arg extends it
/// to a one-step cross-session PANE pick — after selecting the
/// window, the driver focuses its DFS leaf ordinal `P`. The
/// agent-fleet dashboard's foreign pane rows commit this form.
///
/// phux-c2td.3: an optional `host = "NAME"` arg names a federation
/// satellite instead of a session on this server, and takes the
/// [`open_satellite_session`] path — the session picker's satellite
/// rows commit that form.
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
    effects.reattach = Some(ReattachTarget::Existing { name, window, pane });
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
    let Some(target) = satellite_session_pane(ctx.hosts, host, name) else {
        tracing::warn!(
            host,
            session = name,
            "switch-session: no reachable satellite session by that name",
        );
        effects.bell = true;
        return;
    };
    if let Some(index) = window_holding(ctx.workspace, &target) {
        focus_open_satellite_pane(ctx, effects, index, target);
        return;
    }
    if attach_in_flight(ctx.pending_windows, &target) {
        return;
    }
    let request_id = take_request_id(ctx);
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
    effects.command_frames.push(FrameKind::Command {
        request_id,
        command: Command::AttachResource {
            terminal_id: target,
        },
    });
}

/// Focus the window already holding a satellite session's pane.
fn focus_open_satellite_pane(
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

/// Create a fresh session (or attach to one already named) and
/// switch this client to it in-process. An explicit `name`
/// creates it directly; with no name we open a prompt to type
/// one, which commits `new-session { name }` back through this
/// same path. Either way the re-attach uses `CreateIfMissing`.
fn new_session(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
) {
    match name_arg(resolved) {
        Some(name) => effects.reattach = Some(ReattachTarget::Create(name)),
        None => ctx
            .overlays
            .push(Box::new(PromptOverlay::new_session(ctx.theme))),
    }
}

/// phux-r82.5: run a plugin manifest action through the same
/// child-process runtime `phux config run PLUGIN ACTION` uses.
/// Sync dispatch only records the intent; the async caller
/// (`apply_action_effects`) spawns the run off the input loop so
/// a slow plugin never freezes the TUI. Completion arrives on
/// the driver's plugin-events channel; failures toast.
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

/// phux-r82.7: open a plugin manifest `[[panes]]` entry as a
/// real server-side Terminal running the pane's argv. Routes
/// through the SAME `SPAWN_RESOURCE` machinery `split-pane` /
/// `new-window` use (ADR-0017: no plugin-privileged wire
/// surface) — the manifest supplies the command, the plugin
/// root the cwd, and `PHUX_PLUGIN_*` the additive env. Placement
/// picks the parked intent: `split`/`zoomed` park a
/// `PendingSplit` (zoomed also zooms the new pane when the
/// reply lands), `tab` parks a `PendingWindow` named after the
/// pane title. `overlay` entries never reach the snapshot
/// (deferred), so an unknown (plugin, pane) pair here also
/// covers a disabled plugin or an overlay declaration bound
/// directly in user config.
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
            "plugin-pane names no hostable pane (unknown, disabled, or overlay-deferred); dropping",
        );
        effects.bell = true;
        return;
    };
    let request_id = take_request_id(ctx);
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
            };
            set_spawn_initial_size(&mut frame, predicted_split_size(ctx, &pending));
            effects.spawn_terminal = Some((request_id, pending, frame));
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

/// Step the active window's focus with `step` (`next-pane` /
/// `previous-pane`), adopting the resulting layout state.
fn cycle_pane(
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
    step: fn(&LayoutState) -> Option<LayoutState>,
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

/// One-entry MRU jump-back. The target may be in another window;
/// locate it by stable `ResourceId`, switch the client-local active
/// window, and restore that window's local focus. Applying the
/// resulting focus change records the pane we jumped from as the
/// next MRU, so repeated invocations toggle between two panes.
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

/// phux-x2hm: zoom needs more than one pane (a single-pane window
/// bells, like tmux). When already zoomed the REAL tree still has >1
/// leaf, so this same check permits un-zooming. The driver owns
/// the `zoomed` state; we just signal intent + request a repaint.
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

/// Show/hide the window sidebar.
///
/// The strip costs its width off every pane. On a terminal too
/// narrow to afford it and still leave a usable pane area, the
/// driver's reservation folds to `None` — so turning it "on"
/// would change nothing on screen and the keypress would read
/// as broken. Refuse with the bell instead, the same way zoom
/// refuses on a single-pane window: a refusal you can hear
/// beats a toggle that silently does nothing.
///
/// Turning it *off* is always allowed: that direction never
/// needs room, and a user shrinking their terminal must be
/// able to reclaim the columns.
const fn toggle_sidebar(ctx: &DispatchCtx<'_>, effects: &mut ActionEffects) {
    if !*ctx.sidebar_enabled
        && crate::attach::paint::sidebar_reservation(
            ctx.viewport.0,
            true,
            ctx.sidebar_width,
            crate::attach::paint::SidebarEdge::Left,
            ctx.chrome.min_pane_cols,
        )
        .is_none()
    {
        effects.bell = true;
        return;
    }
    // phux-4h5a: show/hide the window sidebar. The driver owns
    // `sidebar_enabled`; we signal intent + a repaint so the panes
    // reflow into/out of the reserved columns.
    // phux-4h5a P4 follow-up: a `focus-window`-by-index action (the
    // keyboard companion to clicking a strip row) is deferred; the
    // existing `select-window` jumps by tab position, but a strip-row
    // index action that pairs with mouse click-to-focus is not yet
    // wired.
    effects.toggle_sidebar = true;
    effects.layout_mutated = true;
}
