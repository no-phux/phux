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
    for window in &workspace.groups {
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
                    format!("{} · pane {}", window.name, pane_index + 1),
                    phux_config::keybind::ResolvedAction {
                        action: "move-pane".to_owned(),
                        args,
                    },
                )
                .secondary(session_name.to_owned()),
            );
        }
    }
}

/// The grouped window picker: one header per session (current first, then
/// by name). Current-session windows commit `select-window { index }`; a
/// peer with a cached layout lists one-step `switch-session { name, window }`
/// rows, else a single "switch to this session" row. The caller bells when
/// only headers result. `terminals` counts each session's terminals in the
/// cached graph (`PeerInputs::terminal_counts`), `Some` only when that graph
/// reports each resource's session (`PeerInputs::reports_sessions`).
pub(super) fn window_picker_items(
    workspace: &ArrangementDoc,
    names: &WindowNames<'_>,
    sessions: &[phux_protocol::wire::info::SessionInfo],
    foreign_layouts: &HashMap<phux_protocol::ids::SessionId, ArrangementDoc>,
    terminals: Option<&HashMap<phux_protocol::ids::SessionId, usize>>,
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
        items.push(SelectItem::header(session.name.clone()));

        if is_current {
            items.extend(current_session_window_rows(workspace, names));
        } else if let Some(foreign) = foreign_layouts
            .get(&session.id)
            .filter(|ws| !ws.groups.is_empty())
        {
            items.extend(foreign_session_window_rows(session, foreign));
        } else {
            // No cached layout for this foreign session; offer a switch.
            let tabs = session_tab_count(session, terminals);
            let windows = if tabs == 1 {
                "1 tab".to_owned()
            } else {
                format!("{tabs} tabs")
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
        items.extend(current_session_window_rows(workspace, names));
    }
    items
}

/// What the picker needs to call a window what the tab bar calls it.
pub(super) struct WindowNames<'a> {
    pub panes: &'a HashMap<ResourceId, crate::attach::pane_state::PaneSlot>,
    pub agent_meta: &'a HashMap<ResourceId, phux_client::agent_meta::AgentRecord>,
}

impl WindowNames<'_> {
    /// The tab bar's label for the window at `index`, with its index in
    /// front, since `C-a <index>` is how the keyboard reaches it.
    fn label(&self, index: usize, window: &crate::layout::Group) -> String {
        let name = crate::attach::pane_state::display_window_name(
            &window.name,
            window.state.focus.as_ref(),
            self.panes,
            self.agent_meta,
        );
        format!("{index} {name}")
    }
}

/// How many tabs a peer session with no cached arrangement opens as.
///
/// When the cached graph reports each resource's session (an
/// `ORGANIZATION` server, ADR-0161 §7), the server's window count is
/// synthesized (one per non-empty session) and attaching gives each of the
/// session's terminals a tab of its own, so the terminal count is the tab
/// count; a session the graph lists no terminal for opens empty, as its
/// synthesized count of 0 said. Before such a graph is cached, or from an
/// older server, the reported window count is all there is.
fn session_tab_count(
    session: &phux_protocol::wire::info::SessionInfo,
    terminals: Option<&HashMap<phux_protocol::ids::SessionId, usize>>,
) -> usize {
    terminals.map_or_else(
        || usize::from(session.window_count),
        |terminals| terminals.get(&session.id).copied().unwrap_or(0),
    )
}

/// The indented, selectable window rows for the locally-attached session,
/// drawn from the client's [`ArrangementDoc`], labelled as the tab bar
/// labels them. Each commits `select-window { index }`.
pub(super) fn current_session_window_rows(
    workspace: &ArrangementDoc,
    names: &WindowNames<'_>,
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
            let label = names.label(index, window);
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
            let label = window.name.clone();
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
    /// Panes of each session that want a person.
    pub needs_you: std::collections::HashMap<phux_protocol::ids::SessionId, usize>,
}

/// Session picker rows, read at a glance (`docs/consumers/attention.md`):
/// `focused` first (committing it is a silent no-op), then the sessions
/// with panes that need you, then the rest by name. Each commits
/// `switch-session { name }`.
pub(super) fn session_picker_items(
    sessions: &[phux_protocol::wire::info::SessionInfo],
    focused: Option<phux_protocol::ids::SessionId>,
    counts: &SessionCounts,
) -> Vec<SelectItem> {
    let mut ordered: Vec<_> = sessions.iter().collect();
    ordered.sort_by(|left, right| session_picker_order(focused, counts, left, right));
    ordered
        .into_iter()
        .map(|session| attached_session_row(session, counts))
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

/// One attached-host session row: its name, and how many of its panes need
/// you when any do.
fn attached_session_row(
    session: &phux_protocol::wire::info::SessionInfo,
    counts: &SessionCounts,
) -> SelectItem {
    let item = SelectItem::new(
        session.name.clone(),
        phux_config::keybind::ResolvedAction {
            action: "switch-session".to_owned(),
            args: switch_session_args(&session.name, Some(session.id)),
        },
    );
    match counts.needs_you.get(&session.id).copied().unwrap_or(0) {
        0 => item,
        waiting => item.secondary(format!("{waiting} need you")),
    }
}

/// Header for this host's group in the host-grouped session picker.
pub(super) const LOCAL_HOST_HEADER: &str = "Local";

/// Live-refresh key: an open session picker is rebuilt when a fresh host
/// inventory or hosts-provider answer lands.
pub(in crate::attach) const SESSION_PICKER_LIVE_KEY: &str = "session-picker";

/// The full session-picker rows (host-grouped sessions plus "+ New
/// session"), one builder for the open and the live refresh.
///
/// A project identity or non-empty project tag regroups only the attached
/// host's sessions. Satellites and other machines stay under their own
/// headers; another machine's row names its project.
pub(in crate::attach) fn session_picker_rows(
    peers: &PeerInputs<'_>,
    workspace: &ArrangementDoc,
) -> Vec<SelectItem> {
    let here = match peers.origin {
        Some(AttachOrigin::Remote(name)) => name.as_str(),
        Some(AttachOrigin::Local) | None => LOCAL_HOST_HEADER,
    };
    let counts = SessionCounts {
        needs_you: crate::attach::sidebar_zones::peer_needs_you(peers),
    };
    let machines = peers.other_machines();
    let grouped = peers
        .sessions
        .iter()
        .any(|session| phux_client::project_identity::of_session(session).is_some())
        || peers.project_tags.values().any(|tag| !tag.is_empty());
    let mut items = if grouped {
        assemble_host_groups(
            project_grouped_session_items(peers.sessions, peers.focused_session, &counts, peers),
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

/// Attached-host sessions grouped by project (ADR-0168): a session's
/// identity, else its `phux.session.project/v1` label. Groups sort by name;
/// in each, primary checkouts lead and their linked worktrees follow marked
/// `└`. A group header counts the panes that need you. Sessions in no
/// project sit under `Sessions`. Each row is an [`attached_session_row`].
fn project_grouped_session_items(
    sessions: &[phux_protocol::wire::info::SessionInfo],
    focused: Option<SessionId>,
    counts: &SessionCounts,
    peers: &PeerInputs<'_>,
) -> Vec<SelectItem> {
    use phux_client::project_groups::{Block, GroupRow, group_rows};
    let mut ordered: Vec<_> = sessions.iter().collect();
    ordered.sort_by(|left, right| session_picker_order(focused, counts, left, right));
    let identities: Vec<_> = ordered
        .iter()
        .map(|session| phux_client::project_identity::of_session(session))
        .collect();
    let rows: Vec<GroupRow<'_>> = ordered
        .iter()
        .zip(&identities)
        .map(|(session, identity)| GroupRow {
            project: identity.as_ref(),
            label: peers.project_tags.get(&session.id).map(String::as_str),
            needs_you: counts.needs_you.get(&session.id).copied().unwrap_or(0),
        })
        .collect();
    let mut groups = Vec::new();
    let mut ungrouped = Vec::new();
    for block in group_rows(&rows, 1) {
        match block {
            Block::Single(index) => ungrouped.push(index),
            Block::Project(block) => groups.push(block),
        }
    }
    groups.sort_by(|left, right| left.name.cmp(&right.name));
    let mut items = Vec::new();
    for group in groups {
        items.push(SelectItem::header(match group.needs_you {
            0 => group.name.clone(),
            n => format!("{} - {n} need you", group.name),
        }));
        for member in group.members {
            let mut row = attached_session_row(ordered[member.index], counts).indented();
            if member.linked {
                row.label = format!("└ {}", row.label);
            }
            items.push(row);
        }
    }
    if !ungrouped.is_empty() {
        items.push(SelectItem::header("Sessions"));
        items.extend(
            ungrouped
                .into_iter()
                .map(|index| attached_session_row(ordered[index], counts).indented()),
        );
    }
    items
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
        return vec![SelectItem::header(format!("{} - unreachable", host.label))];
    }
    let mut items = vec![SelectItem::header(host.label.clone())];
    items.extend(host.sessions.iter().map(|session| {
        // The host stays on the row so it still reads once a typed query
        // hides the headers.
        let mut details = vec![format!("on {}", host.label)];
        if let Some(project) = &session.project {
            details.push(format!("project {}", project.name));
        }
        let exec = &session.exec;
        let needs = exec.failed.saturating_add(exec.lost);
        if needs > 0 {
            details.push(format!("{needs} need you"));
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
    if host.unreachable.is_some() {
        return vec![SelectItem::header(format!("{} - unreachable", host.host))];
    }
    let mut items = vec![SelectItem::header(host.host.to_string())];
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
    let mut details = vec![format!("on {}", host.host)];
    if session
        .active_resource
        .as_ref()
        .is_some_and(|id| window_holding(workspace, id).is_some())
    {
        details.push("open here".to_owned());
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
