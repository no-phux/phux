//! Window/session picker rows and the client-local window switch.

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
use phux_protocol::ids::SessionId;

use crate::layout::Workspace;
use crate::render::overlay::SelectItem;

use super::ctx::DispatchCtx;
use super::effects::ActionEffects;

/// Build exact local destinations for moving the focused pane.
///
/// Only topology the TUI actually has is offered: the attached workspace and
/// foreign workspaces with a complete cached layout. The focused source,
/// satellite leaves, and foreign sessions without a layout cache are omitted.
/// Each row identifies session, window, pane ordinal, and stable local id.
pub(super) fn move_pane_picker_items(
    source: &ResourceId,
    workspace: &Workspace,
    session_name: &str,
    focused_session: Option<SessionId>,
    sessions: &[phux_protocol::wire::info::SessionInfo],
    foreign_layouts: &HashMap<SessionId, Workspace>,
) -> Vec<SelectItem> {
    let mut rows = Vec::new();
    append_move_destinations(&mut rows, source, session_name, workspace);

    let mut foreign: Vec<_> = sessions
        .iter()
        .filter(|session| Some(session.id) != focused_session)
        .filter_map(|session| {
            foreign_layouts
                .get(&session.id)
                .map(|workspace| (session.name.as_str(), workspace))
        })
        .collect();
    foreign.sort_by_key(|(name, _)| *name);
    for (name, workspace) in foreign {
        append_move_destinations(&mut rows, source, name, workspace);
    }
    rows
}

fn append_move_destinations(
    rows: &mut Vec<SelectItem>,
    source: &ResourceId,
    session_name: &str,
    workspace: &Workspace,
) {
    for (window_index, window) in workspace.windows.iter().enumerate() {
        let Some(tree) = &window.state.tree else {
            continue;
        };
        for (pane_index, pane) in crate::layout::leaves(tree).into_iter().enumerate() {
            let ResourceId::Local { id } = pane else {
                continue;
            };
            let pane = ResourceId::Local { id };
            if &pane == source {
                continue;
            }
            let mut args = std::collections::BTreeMap::new();
            args.insert("target".to_owned(), toml::Value::Integer(i64::from(id)));
            rows.push(
                SelectItem::new(
                    format!("@{id}"),
                    phux_config::keybind::ResolvedAction {
                        action: "move-pane".to_owned(),
                        args,
                    },
                )
                .secondary(format!(
                    "{session_name} · {window_index}:{} · pane {}",
                    window.name,
                    pane_index + 1
                )),
            );
        }
    }
}

/// Build the `<leader> w` grouped window picker's rows (phux-4li.19 / nav).
///
/// The picker is hierarchical: one [`SelectItem::header`] per session, with
/// that session's windows nested (indented) beneath it. Sessions are
/// ordered with the **current** session first (so the windows you can act
/// on directly lead), then the rest by name for a stable layout.
///
/// - Under the **current** session, each window row is `index:name` with
///   the pane count as the dimmed secondary; it commits
///   `select-window { index }` — the same per-client window switch the
///   numeric prefix bindings use, routed through the single dispatch path.
/// - Under **other** sessions with a cached persisted layout
///   (`foreign_layouts`, fetched by the driver at attach — phux-foz.8),
///   each window renders the same `index:name` row committing
///   `switch-session { name, window = index }`: one step re-attaches to
///   that session AND selects the window once its layout loads.
/// - A foreign session with **no** cached layout (nothing persisted yet,
///   the GET reply hasn't landed, or the session appeared after attach)
///   falls back to a single "switch to this session" row committing
///   `switch-session { name }` — its own picker then lists its windows.
///
/// Headers are non-selectable; a session with no rows beneath it (the
/// current session with zero windows) still contributes its header, and
/// the caller bells when *only* headers result.
pub(super) fn window_picker_items(
    workspace: &Workspace,
    sessions: &[phux_protocol::wire::info::SessionInfo],
    foreign_layouts: &HashMap<phux_protocol::ids::SessionId, Workspace>,
    focused: Option<phux_protocol::ids::SessionId>,
) -> Vec<SelectItem> {
    // Order sessions: current first, then the rest alphabetically by name
    // for a deterministic layout.
    let mut ordered: Vec<&phux_protocol::wire::info::SessionInfo> = sessions.iter().collect();
    ordered.sort_by(|a, b| {
        let a_cur = Some(a.id) == focused;
        let b_cur = Some(b.id) == focused;
        b_cur.cmp(&a_cur).then_with(|| a.name.cmp(&b.name))
    });

    let mut items = Vec::new();
    for session in ordered {
        let is_current = Some(session.id) == focused;
        let header = if is_current {
            format!("{} (current)", session.name)
        } else {
            session.name.clone()
        };
        items.push(SelectItem::header(header));

        if is_current {
            items.extend(current_session_window_rows(workspace));
        } else if let Some(foreign) = foreign_layouts
            .get(&session.id)
            .filter(|ws| !ws.windows.is_empty())
        {
            // phux-foz.8: the one-step rows. Same `index:name` + pane-count
            // shape as the current session's rows, but committing
            // `switch-session { name, window }` so a single Enter lands in
            // that window of that session.
            items.extend(foreign_session_window_rows(&session.name, foreign));
        } else {
            // No cached layout for this foreign session; offer a switch.
            let windows = if session.window_count == 1 {
                "1 window".to_owned()
            } else {
                format!("{} windows", session.window_count)
            };
            let mut args = std::collections::BTreeMap::new();
            args.insert("name".to_owned(), toml::Value::String(session.name.clone()));
            items.push(
                SelectItem::new(
                    "switch to this session",
                    phux_config::keybind::ResolvedAction {
                        action: "switch-session".to_owned(),
                        args,
                    },
                )
                .secondary(windows)
                .indented(),
            );
        }
    }

    // No sessions cached yet (pre-snapshot): fall back to a flat list of
    // the current workspace's windows so the picker is still useful.
    if items.is_empty() {
        items.extend(current_session_window_rows(workspace));
    }
    items
}

/// The indented, selectable window rows for the locally-attached session,
/// drawn from the client's [`Workspace`]. Each commits
/// `select-window { index }`.
pub(super) fn current_session_window_rows(workspace: &Workspace) -> Vec<SelectItem> {
    workspace
        .windows
        .iter()
        .enumerate()
        .map(|(index, window)| {
            let panes = window
                .state
                .tree
                .as_ref()
                .map_or(0, |tree| crate::layout::leaves(tree).len());
            let label = format!("{index}:{}", window.name);
            let secondary = if panes == 1 {
                "1 pane".to_owned()
            } else {
                format!("{panes} panes")
            };
            let mut args = std::collections::BTreeMap::new();
            // Window counts never approach i64::MAX; the lossless path is
            // the only one that can fire in practice.
            let idx_i64 = i64::try_from(index).unwrap_or(i64::MAX);
            args.insert("index".to_owned(), toml::Value::Integer(idx_i64));
            SelectItem::new(
                label,
                phux_config::keybind::ResolvedAction {
                    action: "select-window".to_owned(),
                    args,
                },
            )
            .secondary(secondary)
            .indented()
        })
        .collect()
}

/// phux-foz.8: the indented one-step jump rows for a **foreign** session,
/// drawn from its cached persisted [`Workspace`] (`DispatchCtx::
/// foreign_layouts`). Same `index:name` + pane-count shape as
/// [`current_session_window_rows`], but each row commits
/// `switch-session { name, window = index }` — the combined
/// re-attach-and-select the driver resolves after the target's layout
/// loads.
pub(super) fn foreign_session_window_rows(
    session_name: &str,
    workspace: &Workspace,
) -> Vec<SelectItem> {
    workspace
        .windows
        .iter()
        .enumerate()
        .map(|(index, window)| {
            let panes = window
                .state
                .tree
                .as_ref()
                .map_or(0, |tree| crate::layout::leaves(tree).len());
            let label = format!("{index}:{}", window.name);
            let secondary = if panes == 1 {
                "1 pane".to_owned()
            } else {
                format!("{panes} panes")
            };
            let mut args = std::collections::BTreeMap::new();
            args.insert(
                "name".to_owned(),
                toml::Value::String(session_name.to_owned()),
            );
            // Window counts never approach i64::MAX; the lossless path is
            // the only one that can fire in practice.
            let idx_i64 = i64::try_from(index).unwrap_or(i64::MAX);
            args.insert("window".to_owned(), toml::Value::Integer(idx_i64));
            SelectItem::new(
                label,
                phux_config::keybind::ResolvedAction {
                    action: "switch-session".to_owned(),
                    args,
                },
            )
            .secondary(secondary)
            .indented()
        })
        .collect()
}

/// Build the session picker's rows from the client's cached
/// session graph (phux-4li.20).
///
/// One row per session, with `focused` first and marked `current`. Each row's
/// label is the session name with a window/attached-client summary as the
/// dimmed secondary. Choosing it commits `switch-session { name }`; the
/// current row dismisses as a silent no-op and peer rows reattach through the
/// same dispatch path.
pub(super) fn session_picker_items(
    sessions: &[phux_protocol::wire::info::SessionInfo],
    focused: Option<phux_protocol::ids::SessionId>,
) -> Vec<SelectItem> {
    let mut ordered: Vec<_> = sessions.iter().collect();
    ordered.sort_by(|a, b| {
        let a_current = Some(a.id) == focused;
        let b_current = Some(b.id) == focused;
        b_current.cmp(&a_current).then_with(|| a.name.cmp(&b.name))
    });

    ordered
        .into_iter()
        .map(|s| {
            let windows = if s.window_count == 1 {
                "1 window".to_owned()
            } else {
                format!("{} windows", s.window_count)
            };
            let mut details = vec![windows];
            if Some(s.id) == focused {
                details.push("current".to_owned());
            }
            if s.attached_client_count != 0 {
                details.push(format!("{} attached", s.attached_client_count));
            }
            let mut args = std::collections::BTreeMap::new();
            args.insert("name".to_owned(), toml::Value::String(s.name.clone()));
            SelectItem::new(
                s.name.clone(),
                phux_config::keybind::ResolvedAction {
                    action: "switch-session".to_owned(),
                    args,
                },
            )
            .secondary(details.join(", "))
        })
        .collect()
}

/// Header for this host's group in the host-grouped session picker.
pub(super) const LOCAL_HOST_HEADER: &str = "Local";

/// Live-refresh key for the session picker: the driver rebuilds its rows
/// when a fresh host inventory lands, so a picker opened before the
/// `GET_STATE` reply fills in its satellites in place instead of showing a
/// stale fleet.
pub(in crate::attach) const SESSION_PICKER_LIVE_KEY: &str = "session-picker";

/// The complete session-picker row set: the host-grouped sessions plus the
/// trailing "+ New session" row.
///
/// One builder so the initial open and the live refresh that lands with a
/// fresh host inventory cannot drift apart.
pub(in crate::attach) fn session_picker_rows(
    sessions: &[phux_protocol::wire::info::SessionInfo],
    focused: Option<phux_protocol::ids::SessionId>,
    hosts: &[phux_protocol::wire::info::HostInventory],
    workspace: &Workspace,
) -> Vec<SelectItem> {
    let mut items = host_grouped_session_items(sessions, focused, hosts, workspace);
    items.push(new_session_item());
    items
}

/// Build the session picker's rows grouped by host (phux-c2td.3).
///
/// With no satellite inventory (`hosts` empty — a non-hub server, or one
/// that predates `ServerFeature::HostSessions`) this is exactly
/// [`session_picker_items`]: an ungrouped list, unchanged. With one, this
/// host's sessions nest under a [`LOCAL_HOST_HEADER`] header and each
/// satellite follows under its own, its sessions committing
/// `switch-session { name, host }` — the same key, one host further out.
///
/// A satellite the hub could not reach keeps its header, marked
/// `(unreachable)`, rather than disappearing: a session that exists but is
/// currently unlistable is exactly the one a user needs told about.
pub(super) fn host_grouped_session_items(
    sessions: &[phux_protocol::wire::info::SessionInfo],
    focused: Option<phux_protocol::ids::SessionId>,
    hosts: &[phux_protocol::wire::info::HostInventory],
    workspace: &Workspace,
) -> Vec<SelectItem> {
    let local = session_picker_items(sessions, focused);
    if hosts.is_empty() {
        return local;
    }
    let mut items = vec![SelectItem::header(LOCAL_HOST_HEADER)];
    items.extend(local.into_iter().map(SelectItem::indented));
    for host in hosts {
        items.extend(satellite_host_items(host, workspace));
    }
    items
}

/// One satellite's header plus its name-sorted session rows.
fn satellite_host_items(
    host: &phux_protocol::wire::info::HostInventory,
    workspace: &Workspace,
) -> Vec<SelectItem> {
    if let Some(reason) = host.unreachable.as_deref() {
        return vec![SelectItem::header(format!(
            "{} - unreachable: {reason}",
            host.host
        ))];
    }
    let status = if host.sessions.is_empty() {
        "connected, no sessions".to_owned()
    } else {
        count_label(
            u16::try_from(host.sessions.len()).unwrap_or(u16::MAX),
            "session",
            "sessions",
        )
    };
    let mut items = vec![SelectItem::header(format!("{} - {status}", host.host))];
    let mut sessions: Vec<_> = host.sessions.iter().collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    items.extend(
        sessions
            .into_iter()
            .map(|session| satellite_session_item(host, session, workspace)),
    );
    items
}

/// One satellite session's row. The label is the session's own name (the
/// header carries the host); the secondary names the host too, so the row
/// still reads correctly once a typed query hides the headers.
fn satellite_session_item(
    host: &phux_protocol::wire::info::HostInventory,
    session: &phux_protocol::wire::info::HostSessionInfo,
    workspace: &Workspace,
) -> SelectItem {
    let mut details = vec![
        format!("on {}", host.host),
        count_label(session.window_count, "window", "windows"),
        count_label(session.pane_count, "pane", "panes"),
    ];
    if session
        .active_resource
        .as_ref()
        .is_some_and(|id| window_holding(workspace, id).is_some())
    {
        details.push("open here".to_owned());
    }
    if session.attached_client_count != 0 {
        details.push(format!("{} attached", session.attached_client_count));
    }
    let mut args = std::collections::BTreeMap::new();
    args.insert("name".to_owned(), toml::Value::String(session.name.clone()));
    args.insert(
        "host".to_owned(),
        toml::Value::String(host.host.to_string()),
    );
    SelectItem::new(
        session.name.clone(),
        phux_config::keybind::ResolvedAction {
            action: "switch-session".to_owned(),
            args,
        },
    )
    .secondary(details.join(", "))
    .indented()
}

/// The index of the first window of this client's workspace holding `id` as
/// a leaf, if any. Behind a satellite session's "open here" marker, and
/// behind `switch-session { name, host }` choosing to focus an already-open
/// satellite pane instead of opening a second window onto it.
pub(super) fn window_holding(
    workspace: &Workspace,
    id: &phux_protocol::ResourceId,
) -> Option<usize> {
    workspace.windows.iter().position(|window| {
        window
            .state
            .tree
            .as_ref()
            .is_some_and(|tree| crate::layout::leaves(tree).contains(id))
    })
}

fn count_label(n: u16, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The trailing "+ New session" row for the session picker. Committing it
/// runs the bare `new-session` action, which opens the name prompt — so a
/// new session is always reachable from `<leader> a`, even when this is
/// the only session.
pub(super) fn new_session_item() -> SelectItem {
    SelectItem::new(
        "+ New session…".to_owned(),
        phux_config::keybind::ResolvedAction {
            action: "new-session".to_owned(),
            args: std::collections::BTreeMap::new(),
        },
    )
    .secondary("create".to_owned())
}

/// Apply a window-switch `mutate` to the workspace and, **only if the
/// active window actually changed**, record the follow-up: repaint the
/// new composition, drop the prediction queue, and move focus to the new
/// active window's focused leaf. A no-op switch (single window, wrap to
/// self, or an out-of-range `select`) leaves `effects` untouched.
///
/// Window selection is per-client like focus (ADR-0019 decision 6), so
/// this emits no `SET_METADATA` — siblings keep their own active window.
pub(super) fn switch_window(
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
    mutate: impl FnOnce(&mut Workspace),
) {
    let before = ctx.workspace.active;
    mutate(ctx.workspace);
    if ctx.workspace.active == before {
        return;
    }
    effects.layout_mutated = true;
    effects.clear_predict = true;
    effects.set_focus = ctx.workspace.active_window().and_then(|w| w.focus.clone());
}
