//! `ResolvedAction` argument parsers and the pure workspace/kill
//! helpers they feed.

//! Input dispatcher: translates parser-emitted events into wire frames
//! or layout-action effects.
//!
//! Owns the resolver-intercept path (prefix chord → `ResolvedAction` →
//! mutate the active window of the `Workspace`), the predict overlay's
//! keystroke feed, and the parked-spawn bookkeeping (`PendingSplit` /
//! `PendingWindow`) that bridges a local `split-pane` / `new-window`
//! chord to its remote `SPAWN_RESOURCE` reply.

use phux_protocol::ResourceId;
use phux_protocol::ids::SessionId;
use phux_protocol::wire::frame::{Command, FrameKind, TerminalSignal};

use crate::layout::{Direction, SplitDir, Workspace};

/// Flatten a workspace deterministically: window order, then DFS leaf order.
pub(super) fn ordered_workspace_panes(workspace: &Workspace) -> Vec<(usize, ResourceId)> {
    workspace
        .windows
        .iter()
        .enumerate()
        .flat_map(|(window, state)| {
            state
                .state
                .tree
                .as_ref()
                .map(crate::layout::leaves)
                .unwrap_or_default()
                .into_iter()
                .map(move |id| (window, id))
        })
        .collect()
}

/// Apply a resolved local focus target without producing shared-layout state.
pub(super) fn focus_terminal(workspace: &mut Workspace, window: usize, target: ResourceId) {
    workspace.select(window);
    if let Some(state) = workspace.active_window_mut() {
        state.focus = Some(target);
    }
}

/// Pull a `Direction` out of a [`phux_config::keybind::ResolvedAction`]'s `direction = "..."`
/// arg.
pub(super) fn direction_arg(resolved: &phux_config::keybind::ResolvedAction) -> Option<Direction> {
    let s = resolved.args.get("direction")?.as_str()?;
    match s {
        "up" => Some(Direction::Up),
        "down" => Some(Direction::Down),
        "left" => Some(Direction::Left),
        "right" => Some(Direction::Right),
        // `split-pane direction=horizontal|vertical` uses a different
        // axis vocabulary; this helper is only for focus/resize.
        _ => None,
    }
}

/// Pull an `amount = N` arg out of a [`phux_config::keybind::ResolvedAction`]. TOML integers
/// decode as `i64`; we clamp to `i16` (the [`actions::apply_resize`]
/// signature). Out-of-range values are silently clamped — a `resize-pane
/// amount = 99999` user binding gets a 32767-cell amount, which the
/// underflow guard inside `apply_resize` then rejects.
#[allow(clippy::cast_possible_truncation)]
pub(super) fn amount_arg(resolved: &phux_config::keybind::ResolvedAction) -> Option<i16> {
    let v = resolved.args.get("amount")?.as_integer()?;
    Some(v.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16)
}

/// Pull a window index out of a [`phux_config::keybind::ResolvedAction`]'s `index = N` arg.
/// Negative or non-integer values yield `None` (the caller bells).
pub(super) fn index_arg(resolved: &phux_config::keybind::ResolvedAction) -> Option<usize> {
    usize_arg(resolved, "index")
}

/// Pull a non-negative integer arg (`key = N`) out of a
/// [`phux_config::keybind::ResolvedAction`] (phux-foz.7: `window` / `pane`
/// on `focus-pane`). Negative or non-integer values yield `None` (the
/// caller bells).
pub(super) fn usize_arg(
    resolved: &phux_config::keybind::ResolvedAction,
    key: &str,
) -> Option<usize> {
    let v = resolved.args.get(key)?.as_integer()?;
    usize::try_from(v).ok()
}

/// Pull a window name out of a [`phux_config::keybind::ResolvedAction`]'s `name = "..."` arg.
pub(super) fn name_arg(resolved: &phux_config::keybind::ResolvedAction) -> Option<String> {
    resolved.args.get("name")?.as_str().map(ToOwned::to_owned)
}

/// Pull a session identity out of a `switch-session` `id = N` arg.
///
/// Names are display labels and can move; the wire [`SessionId`] does not.
/// Callers that painted a roster or picker row stash the id so a rename
/// cannot retarget the click. `None` for a typed name, a satellite hop
/// (host-local ids are not attachable here), or a malformed integer.
pub(super) fn session_id_arg(resolved: &phux_config::keybind::ResolvedAction) -> Option<SessionId> {
    let v = resolved.args.get("id")?.as_integer()?;
    u32::try_from(v).ok().map(SessionId::new)
}

/// Pull a `resource = "@N"` / `"host/@N"` arg out of a `switch-session`.
///
/// Graph-discovered agent rows navigate by this identity instead of a
/// fabricated TUI window/pane index (phux-ah84). `None` for a typed name,
/// a window-only pick, or a malformed selector.
pub(super) fn resource_id_arg(
    resolved: &phux_config::keybind::ResolvedAction,
) -> Option<ResourceId> {
    let raw = str_arg(resolved, "resource")?;
    match phux_client::selector::parse(&raw) {
        Ok(phux_client::selector::Selector::ResourceId(id)) => Some(ResourceId::local(id)),
        Ok(phux_client::selector::Selector::SatelliteResourceId { host, id }) => {
            Some(ResourceId::satellite(host, id))
        }
        _ => None,
    }
}

/// `switch-session` args for a local session: the display name plus the
/// stable id when the caller knows it.
pub(super) fn switch_session_args(
    name: impl Into<String>,
    id: Option<SessionId>,
) -> std::collections::BTreeMap<String, toml::Value> {
    let mut args =
        std::collections::BTreeMap::from([("name".to_owned(), toml::Value::String(name.into()))]);
    if let Some(id) = id {
        args.insert("id".to_owned(), toml::Value::Integer(i64::from(id.get())));
    }
    args
}

/// Pull an arbitrary string arg out of a
/// [`phux_config::keybind::ResolvedAction`] (phux-r82.5: `plugin` /
/// `action` on `plugin-action`).
pub(super) fn str_arg(
    resolved: &phux_config::keybind::ResolvedAction,
    key: &str,
) -> Option<String> {
    resolved.args.get(key)?.as_str().map(ToOwned::to_owned)
}

/// The `mouse` argument of `set-pane` (phux-npb3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaneMouseArg {
    /// Opt the pane back in to client mouse handling.
    On,
    /// Opt the pane out (`set-pane mouse off`, ADR-0048's escape hatch).
    Off,
    /// Flip the pane's current state (the palette default).
    Toggle,
}

/// Pull the `mouse = ...` arg out of a `set-pane` action. Accepts the
/// documented strings (`"on"` / `"off"` / `"toggle"`) and, for TOML
/// ergonomics in keybinding tables, plain booleans (`mouse = false` ≡
/// `"off"`). Anything else yields `None` (the caller bells).
pub(super) fn mouse_arg(resolved: &phux_config::keybind::ResolvedAction) -> Option<PaneMouseArg> {
    match resolved.args.get("mouse")? {
        toml::Value::String(s) => match s.as_str() {
            "on" => Some(PaneMouseArg::On),
            "off" => Some(PaneMouseArg::Off),
            "toggle" => Some(PaneMouseArg::Toggle),
            _ => None,
        },
        toml::Value::Boolean(b) => Some(if *b {
            PaneMouseArg::On
        } else {
            PaneMouseArg::Off
        }),
        _ => None,
    }
}

/// Allow `SplitDir` to be parsed from a `direction = "horizontal|vertical"`
/// arg on a `split-pane` action. Lives here (not in `actions.rs`) so the
/// pure helper module stays free of `ResolvedAction` parsing.
///
/// The `direction` string names the DIVIDER orientation (the tmux mental
/// model the default config documents): `vertical` = a vertical divider,
/// i.e. side-by-side panes, which geometrically is a `SplitDir::Horizontal`
/// (split along the width — see `multi_pane::pane_rects`). `horizontal` = a
/// horizontal divider, i.e. stacked panes = `SplitDir::Vertical`. The
/// names are deliberately crossed here: the user-facing word describes the
/// divider; the internal enum describes the split axis.
pub(super) fn split_dir_arg(resolved: &phux_config::keybind::ResolvedAction) -> Option<SplitDir> {
    let s = resolved.args.get("direction")?.as_str()?;
    match s {
        "horizontal" => Some(SplitDir::Vertical),
        "vertical" => Some(SplitDir::Horizontal),
        _ => None,
    }
}

/// ADR-0033: parse the `signal` arg of a `signal-terminal` action into a
/// [`TerminalSignal`]. Recognises `interrupt` / `freeze` / `resume` /
/// `terminate` / `kill`; returns `None` for a missing or unknown value (the
/// arm bells and drops the action).
pub(super) fn signal_arg(
    resolved: &phux_config::keybind::ResolvedAction,
) -> Option<TerminalSignal> {
    match resolved.args.get("signal")?.as_str()? {
        "interrupt" => Some(TerminalSignal::Interrupt),
        "freeze" => Some(TerminalSignal::Freeze),
        "resume" => Some(TerminalSignal::Resume),
        "terminate" => Some(TerminalSignal::Terminate),
        "kill" => Some(TerminalSignal::Kill),
        _ => None,
    }
}

/// Build the `KILL_RESOURCE` command that closes `target`.
///
/// This used to type `exit\n` into the pane as `INPUT_KEY` events and wait
/// for the shell to notice (phux-4li.12). That only worked when the pane's
/// foreground process was a shell sitting at a prompt: with an editor, a
/// pager, an agent CLI, or a wedged process in the foreground the keystrokes
/// were swallowed and the pane simply never closed. It also could not remove
/// a leaf whose resource was already gone, because there was nothing left to
/// type into. The server closes the resource and broadcasts `RESOURCE_CLOSED`
/// regardless of what the pane is running, which is the tmux `kill-pane`
/// semantics the action's name promises.
///
/// `request_id` correlates the refusal: a `TerminalNotFound` reply proves the
/// leaf is dead, which is how a stale layout leaf gets folded out (see
/// `server_frame::handler::fold_missing_resource`).
pub(super) fn kill_resource_frame(target: &ResourceId, request_id: u32) -> FrameKind {
    FrameKind::Command {
        request_id,
        command: Command::KillResource {
            terminal_id: target.clone(),
            // ADR-0109 idempotency keys are for retried kills across a
            // reconnect; a keystroke-driven kill is sent once.
            operation_id: None,
        },
    }
}
