//! Window/session picker rows and the client-local window switch.

//! Input dispatcher: translates parser-emitted events into wire frames
//! or layout-action effects.
//!
//! Owns the resolver-intercept path (prefix chord → `ResolvedAction` →
//! mutate the active window of the `Workspace`), the predict overlay's
//! keystroke feed, and the parked-spawn bookkeeping (`PendingSplit` /
//! `PendingWindow`) that bridges a local `split-pane` / `new-window`
//! chord to its remote `SPAWN_TERMINAL` reply.

use std::collections::HashMap;

use crate::layout::Workspace;
use crate::render::overlay::SelectItem;

use super::ctx::DispatchCtx;
use super::effects::ActionEffects;

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
