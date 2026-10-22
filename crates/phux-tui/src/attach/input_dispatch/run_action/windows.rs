//! Tab handlers: cycling, closing and reopening views, killing, selecting,
//! moving, and renaming tabs.

use phux_protocol::ResourceId;

use crate::attach::actions;
use crate::attach::input_dispatch::args::{index_arg, kill_resource_frame, name_arg};
use crate::attach::input_dispatch::ctx::DispatchCtx;
use crate::attach::input_dispatch::effects::ActionEffects;
use crate::attach::input_dispatch::pickers::{switch_window, window_holding};
use crate::layout::ArrangementDoc;
use crate::render::overlay::PromptOverlay;

use super::ActionCall;

/// `next-window`: activate the tab after the active one, wrapping.
pub(in crate::attach) fn next_window(ActionCall { ctx, effects, .. }: ActionCall<'_, '_>) {
    switch_window(ctx, effects, ArrangementDoc::next);
}

/// `previous-window`: activate the tab before the active one, wrapping.
pub(in crate::attach) fn previous_window(ActionCall { ctx, effects, .. }: ActionCall<'_, '_>) {
    switch_window(ctx, effects, ArrangementDoc::prev);
}

/// Remove the focused view from the arrangement (MIGRATION §03): the leaf
/// folds out of its window, an emptied window goes, and the terminal keeps
/// running and stays a member of the session. Never a kill, never a detach;
/// with nothing left arranged the empty state offers `reopen-views`.
pub(in crate::attach) fn close_view(
    ActionCall {
        ctx,
        focused,
        effects,
        ..
    }: ActionCall<'_, '_>,
) {
    let Some(target) = focused.cloned() else {
        tracing::warn!("close-view: no focused view to close; dropping action");
        effects.bell = true;
        return;
    };
    let Some(index) = window_holding(ctx.workspace, &target) else {
        effects.bell = true;
        return;
    };
    match actions::apply_terminal_closed(&ctx.workspace.groups[index].state, &target) {
        Ok(state) => ctx.workspace.groups[index].state = state,
        Err(err) => {
            tracing::warn!(error = %err, "close-view: the layout could not fold the view");
            effects.bell = true;
            return;
        }
    }
    ctx.workspace.prune_empty_groups();
    if ctx.zoomed.as_ref() == Some(&target) {
        *ctx.zoomed = None;
    }
    effects.set_focus = ctx.workspace.active_group().and_then(|ls| ls.focus.clone());
    effects.clear_predict = true;
    effects.layout_mutated = true;
    effects.set_metadata = true;
}

/// Show every member of the session that has no view: one window each, the
/// first of them active.
pub(in crate::attach) fn reopen_views(
    ActionCall {
        ctx,
        panes,
        effects,
        ..
    }: ActionCall<'_, '_>,
) {
    let hidden = crate::attach::session_mirror::unarranged_members(
        ctx.session_members,
        ctx.workspace,
        panes,
    );
    if hidden.is_empty() {
        effects.bell = true;
        return;
    }
    let first = ctx.workspace.groups.len();
    for id in hidden {
        let name = ctx.workspace.default_group_name();
        ctx.workspace.add_group(name, id);
    }
    let _ = ctx.workspace.select(first);
    effects.set_focus = ctx.workspace.active_group().and_then(|ls| ls.focus.clone());
    effects.clear_predict = true;
    effects.layout_mutated = true;
    effects.set_metadata = true;
}

/// `kill-window`: without a target set, open the confirm naming how many
/// terminals the active window's kill ends and which are also shown
/// elsewhere; with the `terminals` set the confirm committed, kill exactly
/// those, one correlated `KILL_RESOURCE` each, and let the closes fold the
/// window away.
pub(in crate::attach) fn kill_window(
    ActionCall {
        resolved,
        ctx,
        effects,
        ..
    }: ActionCall<'_, '_>,
) {
    match confirmed_targets(resolved) {
        Some(targets) => kill_targets(ctx, effects, targets),
        None => confirm_kill_window(ctx, effects),
    }
}

/// The confirmed target set, when the action carries one.
fn confirmed_targets(resolved: &phux_config::keybind::ResolvedAction) -> Option<Vec<ResourceId>> {
    let toml::Value::Array(items) = resolved
        .args
        .get(crate::render::overlay::kill_confirm::KILL_TARGETS_ARG)?
    else {
        return None;
    };
    Some(
        items
            .iter()
            .filter_map(toml::Value::as_str)
            .filter_map(|raw| phux_client::selector::parse(raw).ok()?.explicit_id())
            .collect(),
    )
}

fn confirm_kill_window(ctx: &mut DispatchCtx<'_>, effects: &mut ActionEffects) {
    let Some(confirm) = kill_window_confirm(ctx) else {
        tracing::warn!("kill-window: no active window to kill; dropping action");
        effects.bell = true;
        return;
    };
    ctx.overlays.push(Box::new(confirm));
}

/// The confirm for killing the active window's terminals; `None` when no
/// window is active.
pub(in crate::attach::input_dispatch) fn kill_window_confirm(
    ctx: &DispatchCtx<'_>,
) -> Option<crate::render::overlay::KillConfirmOverlay> {
    let leaves = ctx
        .workspace
        .active_group()
        .and_then(|ls| ls.tree.as_ref().map(crate::layout::leaves))
        .unwrap_or_default();
    if leaves.is_empty() {
        return None;
    }
    let active = ctx.workspace.active;
    // A user-given name is worth repeating; an auto-numbered one is not
    // what the tab bar shows, and the confirm is about the active tab
    // anyway ("this tab").
    let window = ctx
        .workspace
        .groups
        .get(active)
        .map(|group| group.name.clone())
        .filter(|name| !crate::attach::pane_state::auto_named(name))
        .unwrap_or_default();
    let shared: Vec<crate::render::overlay::SharedTarget> = leaves
        .iter()
        .filter_map(|leaf| {
            let elsewhere = shown_elsewhere(ctx, active, leaf);
            (!elsewhere.is_empty()).then(|| crate::render::overlay::SharedTarget {
                label: crate::attach::server_frame::pane_label(leaf),
                elsewhere,
            })
        })
        .collect();
    Some(crate::render::overlay::KillConfirmOverlay::new(
        &window, leaves, &shared, ctx.theme,
    ))
}

/// How a message names a tab: a user-given name, else the index the tab bar
/// shows and `C-a <n>` reaches (never the auto-number, which would read as
/// a second, disagreeing index).
fn tab_name(index: usize, name: &str) -> String {
    if crate::attach::pane_state::auto_named(name) {
        index.to_string()
    } else {
        name.to_owned()
    }
}

/// Where else `leaf` is arranged: another window of this session, or a
/// peer session's cached arrangement.
fn shown_elsewhere(ctx: &DispatchCtx<'_>, active: usize, leaf: &ResourceId) -> Vec<String> {
    let holds = |doc: &ArrangementDoc| {
        doc.groups.iter().any(|group| {
            group
                .state
                .tree
                .as_ref()
                .is_some_and(|tree| crate::layout::leaves(tree).contains(leaf))
        })
    };
    let mut out: Vec<String> = ctx
        .workspace
        .groups
        .iter()
        .enumerate()
        .filter(|(index, group)| {
            *index != active
                && group
                    .state
                    .tree
                    .as_ref()
                    .is_some_and(|tree| crate::layout::leaves(tree).contains(leaf))
        })
        .map(|(index, group)| format!("tab {}", tab_name(index, &group.name)))
        .collect();
    let mut sessions: Vec<String> = ctx
        .peers
        .foreign_layouts
        .iter()
        .filter(|(session, doc)| Some(**session) != ctx.peers.focused_session && holds(doc))
        .map(|(session, _)| {
            ctx.peers
                .sessions
                .iter()
                .find(|info| info.id == *session)
                .map_or_else(
                    || format!("session {}", session.get()),
                    |info| format!("session {}", info.name),
                )
        })
        .collect();
    sessions.sort();
    out.extend(sessions);
    out
}

/// Kill exactly `leaves`, one correlated `KILL_RESOURCE` each.
fn kill_targets(ctx: &mut DispatchCtx<'_>, effects: &mut ActionEffects, leaves: Vec<ResourceId>) {
    if leaves.is_empty() {
        effects.bell = true;
        return;
    }
    effects.kill_requests = leaves
        .iter()
        .map(|leaf| (ctx.take_request_id(), leaf.clone()))
        .collect();
    effects.kill_frames = effects
        .kill_requests
        .iter()
        .map(|(request_id, leaf)| kill_resource_frame(leaf, *request_id))
        .collect();
    // Every pane in the window dies at our request;
    // none of those closes is news.
    effects.expected_closes = leaves;
}

/// Switch the client-local active window to an explicit `index` arg.
pub(in crate::attach) fn select_window(
    ActionCall {
        resolved,
        ctx,
        effects,
        ..
    }: ActionCall<'_, '_>,
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

/// Move the active window to `index`, else `delta` slots along, clamped.
/// It stays active; order is shared state, so the move broadcasts.
pub(in crate::attach) fn move_window(
    ActionCall {
        resolved,
        ctx,
        effects,
        ..
    }: ActionCall<'_, '_>,
) {
    let from = ctx.workspace.active;
    let last = ctx.workspace.groups.len().saturating_sub(1);
    let target = index_arg(resolved)
        .map(|index| index.min(last))
        .or_else(|| {
            let delta = resolved.args.get("delta")?.as_integer()?;
            Some(offset_index(from, delta, ctx.workspace.groups.len()))
        });
    let Some(to) = target else {
        tracing::warn!(args = ?resolved.args, "move-window needs `index` or `delta`");
        effects.bell = true;
        return;
    };
    if !ctx.workspace.move_group(from, to) {
        effects.bell = true;
        return;
    }
    effects.layout_mutated = true;
    effects.set_metadata = true;
}

/// `from` moved `delta` slots, clamped to `0..len`.
fn offset_index(from: usize, delta: i64, len: usize) -> usize {
    let last = len.saturating_sub(1);
    let magnitude = usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX);
    if delta < 0 {
        from.saturating_sub(magnitude)
    } else {
        from.saturating_add(magnitude).min(last)
    }
}

/// Rename the active window, directly or through the interactive prompt.
pub(in crate::attach) fn rename_window(
    ActionCall {
        resolved,
        ctx,
        effects,
        ..
    }: ActionCall<'_, '_>,
) {
    if ctx.workspace.active_group().is_none() {
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
        // `rename-window` with the typed name.
        let current = ctx
            .workspace
            .groups
            .get(ctx.workspace.active)
            .map(|w| w.name.clone())
            .unwrap_or_default();
        ctx.overlays
            .push(Box::new(PromptOverlay::rename_window(&current, ctx.theme)));
        effects.layout_mutated = true;
    }
}
