//! Window/session picker rows and the client-local window switch.

use std::collections::HashMap;

use phux_protocol::ResourceId;
use phux_protocol::ids::SessionId;

use crate::layout::ArrangementDoc;
use crate::render::overlay::SelectItem;

use super::args::switch_session_args;
use super::ctx::DispatchCtx;
use super::effects::ActionEffects;

mod sessions;
#[cfg(test)]
pub(super) use sessions::{
    LOCAL_HOST_HEADER, SessionCounts, host_grouped_session_items, session_picker_items,
};
pub(in crate::attach) use sessions::{SESSION_PICKER_LIVE_KEY, session_picker_rows};

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
