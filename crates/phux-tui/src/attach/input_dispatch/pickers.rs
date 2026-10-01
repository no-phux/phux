//! Window/session picker rows and the client-local window switch.

use std::collections::HashMap;

use phux_protocol::ResourceId;
use phux_protocol::ids::SessionId;

use phux_core::host_list::HostJson;

use crate::attach::hosts::AttachOrigin;
use crate::attach::sidebar_zones::PeerInputs;
use crate::layout::Workspace;
use crate::render::overlay::SelectItem;

use super::args::{switch_host_action, switch_session_args};
use super::ctx::DispatchCtx;
use super::effects::ActionEffects;

/// Exact local destinations for moving the focused pane: panes of this
/// workspace and of foreign sessions with a cached layout, excluding the
/// source and satellite leaves.
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

/// The grouped window picker: one header per session (current first, then
/// by name). Current-session windows commit `select-window { index }`; a
/// peer with a cached layout lists one-step `switch-session { name, window }`
/// rows, else a single "switch to this session" row. The caller bells when
/// only headers result.
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
            items.extend(foreign_session_window_rows(session, foreign));
        } else {
            // No cached layout for this foreign session; offer a switch.
            let windows = if session.window_count == 1 {
                "1 window".to_owned()
            } else {
                format!("{} windows", session.window_count)
            };
            let args = switch_session_args(&session.name, Some(session.id));
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

/// One-step `switch-session { name, window }` rows for a foreign session's
/// cached layout, shaped like [`current_session_window_rows`].
pub(super) fn foreign_session_window_rows(
    session: &phux_protocol::wire::info::SessionInfo,
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
            let mut args = switch_session_args(&session.name, Some(session.id));
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

/// Session picker rows: `focused` first and marked current (committing it is
/// a silent no-op), each committing `switch-session { name }`.
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
            let args = switch_session_args(&s.name, Some(s.id));
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

/// Live-refresh key: an open session picker is rebuilt when a fresh host
/// inventory or hosts-provider answer lands.
pub(in crate::attach) const SESSION_PICKER_LIVE_KEY: &str = "session-picker";

/// The full session-picker rows (host-grouped sessions plus "+ New
/// session"), one builder for the open and the live refresh.
pub(in crate::attach) fn session_picker_rows(
    peers: &PeerInputs<'_>,
    workspace: &Workspace,
) -> Vec<SelectItem> {
    let here = match peers.origin {
        Some(AttachOrigin::Remote(name)) => name.as_str(),
        Some(AttachOrigin::Local) | None => LOCAL_HOST_HEADER,
    };
    let mut items = host_grouped_session_items(
        peers.sessions,
        peers.focused_session,
        here,
        peers.hosts,
        &peers.other_machines(),
        workspace,
    );
    items.push(new_session_item());
    items
}

/// Session picker rows grouped by host: with no other host exactly
/// [`session_picker_items`]; otherwise the attached server's sessions under
/// `here`, then each hub satellite and each other machine under its own
/// header (kept, marked unreachable, when it could not be listed). A
/// satellite row commits `switch-session { name, host }`; a machine row
/// commits `switch-host { host, name }` (ADR-0140).
pub(super) fn host_grouped_session_items(
    sessions: &[phux_protocol::wire::info::SessionInfo],
    focused: Option<phux_protocol::ids::SessionId>,
    here: &str,
    satellites: &[phux_protocol::wire::info::HostInventory],
    machines: &[&HostJson],
    workspace: &Workspace,
) -> Vec<SelectItem> {
    let local = session_picker_items(sessions, focused);
    if satellites.is_empty() && machines.is_empty() {
        return local;
    }
    let mut items = vec![SelectItem::header(here)];
    items.extend(local.into_iter().map(SelectItem::indented));
    for host in satellites {
        items.extend(satellite_host_items(host, workspace));
    }
    for host in machines {
        items.extend(machine_items(host));
    }
    items
}

/// ADR-0140: one hosts-provider machine's header plus its session rows, in
/// the provider's (name-sorted) order.
fn machine_items(host: &HostJson) -> Vec<SelectItem> {
    if !host.reachable {
        let header = host.error.as_deref().map_or_else(
            || format!("{} - unreachable", host.label),
            |reason| format!("{} - unreachable: {reason}", host.label),
        );
        return vec![SelectItem::header(header)];
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
    let mut items = vec![SelectItem::header(format!("{} - {status}", host.label))];
    items.extend(host.sessions.iter().map(|session| {
        let mut details = vec![
            format!("on {}", host.label),
            count_label(session.windows, "window", "windows"),
        ];
        if session.attached_clients != 0 {
            details.push(format!("{} attached", session.attached_clients));
        }
        SelectItem::new(
            session.name.clone(),
            switch_host_action(host.name.clone(), session.name.clone()),
        )
        .secondary(details.join(", "))
        .indented()
    }));
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

/// The first window holding `id` as a leaf, if any.
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

/// The trailing "+ New session" row (opens the name prompt).
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

/// Apply a window-switch `mutate`; only when the active window changed,
/// repaint, drop predictions, and move focus. Per-client (ADR-0019): no
/// `SET_METADATA`.
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
