//! Window/session picker rows and the client-local window switch.

use std::collections::HashMap;

use phux_protocol::ResourceId;
use phux_protocol::ids::SessionId;

use phux_core::host_list::HostJson;

use crate::attach::hosts::AttachOrigin;
use crate::attach::sidebar_zones::PeerInputs;
use crate::layout::ArrangementDoc;
use crate::render::overlay::SelectItem;

use super::args::{switch_host_action, switch_session_args};
use super::ctx::DispatchCtx;
use super::effects::ActionEffects;

/// Exact local destinations for moving the focused pane: panes of this
/// workspace and of foreign sessions with a cached layout, excluding the
/// source and satellite leaves.
pub(super) fn move_pane_picker_items(
    source: &ResourceId,
    workspace: &ArrangementDoc,
    session_name: &str,
    focused_session: Option<SessionId>,
    sessions: &[phux_protocol::wire::info::SessionInfo],
    foreign_layouts: &HashMap<SessionId, ArrangementDoc>,
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
    workspace: &ArrangementDoc,
) {
    for (window_index, window) in workspace.groups.iter().enumerate() {
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
    workspace: &ArrangementDoc,
    sessions: &[phux_protocol::wire::info::SessionInfo],
    foreign_layouts: &HashMap<phux_protocol::ids::SessionId, ArrangementDoc>,
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
            .filter(|ws| !ws.groups.is_empty())
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
/// drawn from the client's [`ArrangementDoc`]. Each commits
/// `select-window { index }`.
pub(super) fn current_session_window_rows(workspace: &ArrangementDoc) -> Vec<SelectItem> {
    workspace
        .groups
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
    workspace: &ArrangementDoc,
) -> Vec<SelectItem> {
    workspace
        .groups
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

/// Per-session counts a session picker row reads.
#[derive(Debug, Default)]
pub(super) struct SessionCounts {
    /// Terminals each session holds in the cached graph.
    pub terminals: std::collections::HashMap<phux_protocol::ids::SessionId, usize>,
    /// Panes of each session that want a person.
    pub needs_you: std::collections::HashMap<phux_protocol::ids::SessionId, usize>,
}

/// Session picker rows, read at a glance (`docs/consumers/attention.md`):
/// `focused` first and marked current (committing it is a silent no-op),
/// then the sessions with panes that need you, then the rest by name. Each
/// commits `switch-session { name }`. A session's lifetime shows only when
/// it is ephemeral.
pub(super) fn session_picker_items(
    sessions: &[phux_protocol::wire::info::SessionInfo],
    focused: Option<phux_protocol::ids::SessionId>,
    counts: &SessionCounts,
) -> Vec<SelectItem> {
    let mut ordered: Vec<_> = sessions.iter().collect();
    ordered.sort_by(|left, right| session_picker_order(focused, counts, left, right));
    ordered
        .into_iter()
        .map(|session| attached_session_row(session, focused, counts))
        .collect()
}

/// Focused session first, then sessions with panes that need you, then name.
/// Equal names keep their input order.
fn session_picker_order(
    focused: Option<SessionId>,
    counts: &SessionCounts,
    left: &phux_protocol::wire::info::SessionInfo,
    right: &phux_protocol::wire::info::SessionInfo,
) -> std::cmp::Ordering {
    let needs = |id: SessionId| counts.needs_you.get(&id).copied().unwrap_or(0);
    let left_current = Some(left.id) == focused;
    let right_current = Some(right.id) == focused;
    right_current
        .cmp(&left_current)
        .then_with(|| (needs(left.id) == 0).cmp(&(needs(right.id) == 0)))
        .then_with(|| left.name.cmp(&right.name))
}

/// One attached-host session row: panes that need you, terminal count, an
/// ephemeral lifetime, the current marker, and attached clients.
fn attached_session_row(
    session: &phux_protocol::wire::info::SessionInfo,
    focused: Option<SessionId>,
    counts: &SessionCounts,
) -> SelectItem {
    let mut details = Vec::new();
    let waiting = counts.needs_you.get(&session.id).copied().unwrap_or(0);
    if waiting > 0 {
        details.push(format!("{waiting} need you"));
    }
    if let Some(&count) = counts.terminals.get(&session.id) {
        details.push(count_label(
            u16::try_from(count).unwrap_or(u16::MAX),
            "terminal",
            "terminals",
        ));
    }
    if crate::attach::session_mirror::session_lifetime(session)
        == phux_protocol::wire::frame::SessionLifetime::Ephemeral
    {
        details.push("ephemeral".to_owned());
    }
    if Some(session.id) == focused {
        details.push("current".to_owned());
    }
    if session.attached_client_count != 0 {
        details.push(format!("{} attached", session.attached_client_count));
    }
    SelectItem::new(
        session.name.clone(),
        phux_config::keybind::ResolvedAction {
            action: "switch-session".to_owned(),
            args: switch_session_args(&session.name, Some(session.id)),
        },
    )
    .secondary(details.join(", "))
}

/// Header for this host's group in the host-grouped session picker.
pub(super) const LOCAL_HOST_HEADER: &str = "Local";

/// Live-refresh key: an open session picker is rebuilt when a fresh host
/// inventory or hosts-provider answer lands.
pub(in crate::attach) const SESSION_PICKER_LIVE_KEY: &str = "session-picker";

/// The full session-picker rows (host-grouped sessions plus "+ New
/// session"), one builder for the open and the live refresh.
///
/// A non-empty project tag regroups only the attached host's sessions.
/// Satellites and other machines stay under their own headers.
pub(in crate::attach) fn session_picker_rows(
    peers: &PeerInputs<'_>,
    workspace: &ArrangementDoc,
) -> Vec<SelectItem> {
    let here = match peers.origin {
        Some(AttachOrigin::Remote(name)) => name.as_str(),
        Some(AttachOrigin::Local) | None => LOCAL_HOST_HEADER,
    };
    let counts = SessionCounts {
        terminals: peers.terminal_counts(),
        needs_you: crate::attach::sidebar_zones::peer_needs_you(peers),
    };
    let machines = peers.other_machines();
    let mut items = if peers.project_tags.values().any(|tag| !tag.is_empty()) {
        assemble_host_groups(
            project_grouped_session_items(
                peers.sessions,
                peers.focused_session,
                &counts,
                peers.project_tags,
            ),
            here,
            peers.hosts,
            &machines,
            workspace,
        )
    } else {
        host_grouped_session_items(
            peers.sessions,
            peers.focused_session,
            &counts,
            here,
            peers.hosts,
            &machines,
            workspace,
        )
    };
    items.push(new_session_item());
    items
}

/// Attached-host sessions grouped by `phux.session.project/v1`. Untagged
/// sessions sit under `Sessions`. Each row is an [`attached_session_row`],
/// joined by session id because names are not unique.
fn project_grouped_session_items(
    sessions: &[phux_protocol::wire::info::SessionInfo],
    focused: Option<SessionId>,
    counts: &SessionCounts,
    project_tags: &HashMap<SessionId, String>,
) -> Vec<SelectItem> {
    let mut ordered: Vec<_> = sessions.iter().collect();
    ordered.sort_by(|left, right| session_picker_order(focused, counts, left, right));
    let lines: Vec<phux_client_core::organization::SessionLine> = ordered
        .iter()
        .map(|session| phux_client_core::organization::SessionLine {
            name: project_line_name(session.id),
            host: None,
            project: project_tags.get(&session.id).cloned(),
        })
        .collect();
    let mut items = Vec::new();
    for group in phux_client_core::organization::group_by_project(&lines) {
        let header = group
            .project
            .clone()
            .unwrap_or_else(|| "Sessions".to_owned());
        items.push(SelectItem::header(header));
        for line in group.sessions {
            let Some(session) = session_for_project_line(sessions, &line.name) else {
                continue;
            };
            items.push(attached_session_row(session, focused, counts).indented());
        }
    }
    items
}

/// Join key for [`phux_client_core::organization::group_by_project`].
/// `SessionLine` has no id, and the display name is not unique.
fn project_line_name(id: SessionId) -> String {
    format!("id:{}", id.get())
}

fn session_for_project_line<'a>(
    sessions: &'a [phux_protocol::wire::info::SessionInfo],
    name: &str,
) -> Option<&'a phux_protocol::wire::info::SessionInfo> {
    let raw = name.strip_prefix("id:")?;
    let id = raw.parse::<u32>().ok()?;
    sessions
        .iter()
        .find(|session| session.id == SessionId::new(id))
}

/// Session picker rows grouped by host: with no other host exactly
/// [`session_picker_items`]; otherwise the attached server's sessions under
/// `here`, then each hub satellite and each other machine under its own
/// header (kept, marked unreachable, when it could not be listed). A
/// satellite row commits `switch-session { name, host, id }` (`id` is the
/// satellite-local session id); a machine row commits `switch-host { host, name }`
/// (ADR-0140).
pub(super) fn host_grouped_session_items(
    sessions: &[phux_protocol::wire::info::SessionInfo],
    focused: Option<phux_protocol::ids::SessionId>,
    counts: &SessionCounts,
    here: &str,
    satellites: &[phux_protocol::wire::info::HostInventory],
    machines: &[&HostJson],
    workspace: &ArrangementDoc,
) -> Vec<SelectItem> {
    assemble_host_groups(
        session_picker_items(sessions, focused, counts),
        here,
        satellites,
        machines,
        workspace,
    )
}

/// The attached-host block, then each satellite and other machine. No other
/// host returns the block unchanged.
fn assemble_host_groups(
    local: Vec<SelectItem>,
    here: &str,
    satellites: &[phux_protocol::wire::info::HostInventory],
    machines: &[&HostJson],
    workspace: &ArrangementDoc,
) -> Vec<SelectItem> {
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
        let mut details = vec![format!("on {}", host.label)];
        // A server that reports lifetimes counts what runs and what needs
        // you; an older one counts its windows.
        match session.lifetime {
            Some(lifetime) => {
                let exec = &session.exec;
                let needs = exec.failed.saturating_add(exec.lost);
                if needs > 0 {
                    details.push(format!("{needs} need you"));
                }
                let running = exec
                    .running
                    .saturating_add(exec.starting)
                    .saturating_add(exec.stopping);
                if running > 0 {
                    details.push(format!("{running} running"));
                }
                if lifetime == phux_core::SessionLifetime::Ephemeral {
                    details.push("ephemeral".to_owned());
                }
            }
            None => details.push(count_label(session.windows, "window", "windows")),
        }
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
    workspace: &ArrangementDoc,
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
    workspace: &ArrangementDoc,
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
    // Satellite-local id. The row still opens by name + host; the id is
    // what keeps the picker selection on this session across a rename.
    args.insert(
        "id".to_owned(),
        toml::Value::Integer(i64::from(session.id.get())),
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
    workspace: &ArrangementDoc,
    id: &phux_protocol::ResourceId,
) -> Option<usize> {
    workspace.groups.iter().position(|window| {
        window
            .state
            .tree
            .as_ref()
            .is_some_and(|tree| crate::layout::leaves(tree).contains(id))
    })
}

/// How the session picker names a session's lifetime.
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
    mutate: impl FnOnce(&mut ArrangementDoc),
) {
    let before = ctx.workspace.active;
    mutate(ctx.workspace);
    if ctx.workspace.active == before {
        return;
    }
    effects.layout_mutated = true;
    effects.clear_predict = true;
    effects.set_focus = ctx.workspace.active_group().and_then(|w| w.focus.clone());
}
