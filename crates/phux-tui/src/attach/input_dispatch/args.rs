//! `ResolvedAction` argument parsers and the pure workspace/kill
//! helpers they feed.

use phux_protocol::ResourceId;
use phux_protocol::ids::SessionId;
use phux_protocol::wire::frame::{Command, FrameKind, TerminalSignal};

use crate::layout::{Direction, SplitDir, Workspace};

/// A [`phux_config::keybind::ResolvedAction`] with no args.
pub(super) fn bare_action(action: &str) -> phux_config::keybind::ResolvedAction {
    phux_config::keybind::ResolvedAction {
        action: action.to_owned(),
        args: std::collections::BTreeMap::new(),
    }
}

/// `select-window { index }`.
pub(super) fn select_window_action(index: usize) -> Option<phux_config::keybind::ResolvedAction> {
    let mut action = bare_action("select-window");
    action.args.insert(
        "index".to_owned(),
        toml::Value::Integer(i64::try_from(index).ok()?),
    );
    Some(action)
}

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

/// The `amount = N` arg, clamped to `i16` (an absurd amount then fails the
/// resize underflow guard).
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

/// A non-negative integer arg `key = N`; anything else is `None`.
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

/// The `switch-session` `id = N` arg: a painted row's stable session id, so
/// a rename cannot retarget the click.
pub(super) fn session_id_arg(resolved: &phux_config::keybind::ResolvedAction) -> Option<SessionId> {
    let v = resolved.args.get("id")?.as_integer()?;
    u32::try_from(v).ok().map(SessionId::new)
}

/// The `switch-session` `resource = "@N" | "host/@N"` arg, for rows that
/// navigate by pane identity rather than TUI indices.
pub(super) fn resource_id_arg(
    resolved: &phux_config::keybind::ResolvedAction,
) -> Option<ResourceId> {
    let raw = str_arg(resolved, "resource")?;
    phux_client::selector::parse(&raw).ok()?.explicit_id()
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

/// The `mouse` argument of `set-pane`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaneMouseArg {
    /// Opt the pane back in to client mouse handling.
    On,
    /// Opt the pane out (`set-pane mouse off`, ADR-0048's escape hatch).
    Off,
    /// Flip the pane's current state (the palette default).
    Toggle,
}

/// The `set-pane` `mouse` arg: `on`/`off`/`toggle` or a boolean.
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

/// The `split-pane` `direction` arg. It names the DIVIDER (tmux wording):
/// `vertical` means side-by-side panes, which is `SplitDir::Horizontal`;
/// `horizontal` means stacked, `SplitDir::Vertical`.
pub(super) fn split_dir_arg(resolved: &phux_config::keybind::ResolvedAction) -> Option<SplitDir> {
    let s = resolved.args.get("direction")?.as_str()?;
    match s {
        "horizontal" => Some(SplitDir::Vertical),
        "vertical" => Some(SplitDir::Horizontal),
        _ => None,
    }
}

/// ADR-0033: the `signal-terminal` `signal` arg.
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

/// The correlated `KILL_RESOURCE` that closes `target` whatever it runs
/// (tmux `kill-pane`). A `TerminalNotFound` refusal proves a stale leaf is
/// dead so it can be folded out.
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
