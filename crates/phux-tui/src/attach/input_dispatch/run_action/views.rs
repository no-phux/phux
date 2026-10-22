//! Overlays and views: the command palette, pickers, settings, copy mode,
//! the agent fleet and attention, and sidebar filters.

use phux_protocol::ResourceId;

use crate::attach::copy::extract_selection_text;
use crate::attach::input_dispatch::args::{
    focus_terminal, ordered_workspace_panes, str_arg, usize_arg,
};
use crate::attach::input_dispatch::dispatch::{
    focused_pane_rect, open_context_menu, terminal_in_alt_screen, terminal_wants_mouse_tracking,
};
use crate::attach::input_dispatch::pickers::{
    SESSION_PICKER_LIVE_KEY, WindowNames, session_picker_rows, window_picker_items,
};
use crate::attach::pane_state::published_terminal;
use crate::render::overlay::{CopyRequest, SelectItem, SelectList, SelectionGrab, ToastOverlay};

use super::{ActionCall, focus_in_window};

/// Open the single fuzzy discovery surface. `show-help` and
/// `command-palette` are entry aliases so users never have to choose between
/// a reference modal and an executable finder. The recording marker row
/// shows only while a recording is live.
pub(in crate::attach) fn push_action_finder(ActionCall { ctx, .. }: ActionCall<'_, '_>) {
    let mut items = crate::attach::action_registry::palette_items(
        ctx.keybindings,
        ctx.plugin_actions,
        ctx.plugin_panes,
    );
    if !phux_record::recording_live() {
        items.retain(|item| item.action.action != "rec-mark");
    }
    ctx.overlays
        .push(Box::new(SelectList::new("Commands", items, ctx.theme)));
}

/// Explicit reload; the driver re-reads after the batch because `ctx`
/// borrows exactly the state a reload replaces.
pub(in crate::attach) const fn reload_config(ActionCall { effects, .. }: ActionCall<'_, '_>) {
    effects.reload_config = true;
}

/// ADR-0101: open the settings page; a saved edit returns as `reload-config`.
pub(in crate::attach) fn push_settings(ActionCall { ctx, .. }: ActionCall<'_, '_>) {
    ctx.overlays
        .push(Box::new(crate::render::overlay::SettingsOverlay::open(
            phux_config::loader::config_path(),
            ctx.theme,
        )));
}

/// Post an asciicast `m` marker to every recording this machine has open.
///
/// The bare chord writes an empty label. Nothing is recording when the
/// inbox directory has no live file, and that bells.
pub(in crate::attach) fn rec_mark(
    ActionCall {
        resolved, effects, ..
    }: ActionCall<'_, '_>,
) {
    let label = str_arg(resolved, "label").unwrap_or_default();
    match phux_record::post_mark(&label) {
        Ok(0) | Err(_) => effects.bell = true,
        Ok(_) => {}
    }
}

/// Capture a local bug-report bundle, copy its path (OSC 52), and toast it.
/// A write failure bells and toasts instead of panicking.
pub(in crate::attach) fn report_bug(
    ActionCall {
        resolved,
        ctx,
        focused,
        effects,
        ..
    }: ActionCall<'_, '_>,
) {
    let screen = focused.and_then(|id| {
        let terminal = published_terminal(ctx.engine_kernel, id)?;
        extract_selection_text(
            terminal,
            CopyRequest {
                start_row: 0,
                start_col: 0,
                end_row: 0,
                end_col: 0,
                mouse_anchor_screen: None,
                rectangle: false,
                cursor_row: 0,
                cursor_col: 0,
                grab: SelectionGrab::All,
            },
        )
    });
    let (alt_screen, mouse_tracking) = focused
        .and_then(|id| published_terminal(ctx.engine_kernel, id))
        .map(|terminal| {
            (
                terminal_in_alt_screen(terminal),
                terminal_wants_mouse_tracking(terminal),
            )
        })
        .unzip();
    let session = {
        let name = ctx.session_name.trim();
        (!name.is_empty()).then(|| name.to_owned())
    };
    let draft = crate::report::ReportDraft {
        note: str_arg(resolved, "note"),
        session,
        pane: focused.map(pane_selector),
        window: Some(ctx.workspace.active),
        viewport: Some(ctx.viewport),
        alt_screen,
        mouse_tracking,
        screen,
        version: env!("CARGO_PKG_VERSION").to_owned(),
    };
    match crate::report::write_bundle(&draft) {
        Ok(written) => {
            effects.clipboard = Some(written.dir.display().to_string());
            ctx.overlays.push(Box::new(ToastOverlay::passthrough(
                "Bug report",
                vec!["Saved; path copied.".to_owned()],
                ctx.theme,
            )));
        }
        Err(err) => {
            tracing::warn!(error = %err, "report-bug: could not write bundle");
            effects.bell = true;
            ctx.overlays.push(Box::new(ToastOverlay::new(
                "Bug report failed",
                vec![err.to_string()],
                ctx.theme,
            )));
        }
    }
}

fn pane_selector(id: &ResourceId) -> String {
    match id {
        ResourceId::Local { id } => format!("@{id}"),
        ResourceId::Satellite { host, id } => format!("{host}/@{id}"),
    }
}

/// Push the first-run onboarding hint card.
pub(in crate::attach) fn push_getting_started(ActionCall { ctx, .. }: ActionCall<'_, '_>) {
    ctx.overlays
        .push(Box::new(crate::render::overlay::ToastOverlay::passthrough(
            crate::attach::onboarding::ONBOARDING_TITLE,
            crate::attach::onboarding::hint_lines(ctx.keybindings, *ctx.sidebar_enabled),
            ctx.theme,
        )));
}

/// phux-wave-a-copy-mode: enter selection/copy mode. Arrow keys move
/// the cursor without extending the selection unless Shift is held;
/// mouse drag can select and copy in one gesture. The copy cursor starts
/// where the pane's own cursor was last painted (the prompt, usually), so
/// the output just above it is a few keystrokes away; a hidden or
/// scrolled-off cursor starts at the top-left.
pub(in crate::attach) fn push_copy_mode(
    ActionCall {
        ctx,
        focused,
        panes,
        ..
    }: ActionCall<'_, '_>,
) {
    let pane_rect = focused_pane_rect(ctx, focused);
    let (row, col) = focused
        .and_then(|id| panes.get(id))
        .and_then(|slot| slot.renderer.last_cursor_local())
        .unwrap_or((0, 0));
    let overlay = Box::new(crate::render::overlay::CopyModeOverlay::new(
        row,
        col,
        pane_rect.w,
        pane_rect.h,
    ));
    ctx.overlays.push(overlay);
}

/// ADR-0058: the keyboard route to the pane menu (the only one when the app
/// owns the mouse), anchored inside the focused pane's corner.
pub(in crate::attach) fn push_context_menu(ActionCall { ctx, focused, .. }: ActionCall<'_, '_>) {
    let rect = focused_pane_rect(ctx, focused);
    let anchor = (rect.x.saturating_add(2), rect.y.saturating_add(1));
    let zoomed = ctx.zoomed.is_some();
    let spec = crate::attach::context_menu::pane_menu(ctx.keybindings, zoomed);
    open_context_menu(ctx, spec, anchor);
}

/// Push the grouped window picker (sessions as headers, one-step rows for
/// peers with cached layouts). Bells with no rows.
pub(in crate::attach) fn push_window_picker(
    ActionCall {
        ctx,
        effects,
        panes,
        ..
    }: ActionCall<'_, '_>,
) {
    let names = WindowNames {
        panes,
        agent_meta: ctx.agent_meta,
    };
    let items = window_picker_items(
        ctx.workspace,
        &names,
        ctx.peers.sessions,
        ctx.peers.foreign_layouts,
        ctx.peers
            .reports_sessions()
            .then(|| ctx.peers.terminal_counts())
            .as_ref(),
        ctx.peers.focused_session,
    );
    if items.iter().all(SelectItem::is_header) {
        effects.bell = true;
        return;
    }
    ctx.overlays
        .push(Box::new(SelectList::new("Tabs", items, ctx.theme)));
}

/// Push the session picker (grouped by host when a hub satellite or another
/// machine is known), with a "+ New session" row, and ask the driver for a
/// fresh host inventory.
pub(in crate::attach) fn push_session_picker(ActionCall { ctx, .. }: ActionCall<'_, '_>) {
    let items = session_picker_rows(&ctx.peers, ctx.workspace);
    *ctx.host_refresh_request = true;
    ctx.overlays.push(Box::new(
        SelectList::new("Sessions", items, ctx.theme)
            .with_live_key(SESSION_PICKER_LIVE_KEY)
            .with_session_management(),
    ));
}

/// Push the live agent-fleet dashboard: every pane with its agent record,
/// attention, and branch; peers with cached layouts list one-step rows.
/// Bells with nothing to list.
pub(in crate::attach) fn push_agent_fleet(
    ActionCall {
        ctx,
        panes,
        effects,
        ..
    }: ActionCall<'_, '_>,
) {
    let meta = crate::attach::fleet::collect_pane_meta(
        panes,
        ctx.vcs,
        &crate::attach::agent_rows::agent_session_rows(ctx.engine_kernel),
    );
    let mut items = crate::attach::fleet::fleet_items(
        ctx.workspace,
        ctx.peers.sessions,
        ctx.peers.focused_session,
        ctx.agent_meta,
        &meta,
        crate::attach::fleet::ForeignSessions {
            layouts: ctx.peers.foreign_layouts,
            agents: ctx.peers.foreign_agents,
            terminals: &ctx.peers.terminal_counts(),
        },
    );
    items.extend(crate::attach::fleet::satellite_agent_items(
        ctx.peers.foreign_agents,
        ctx.peers.foreign_attention,
        ctx.workspace,
    ));
    if items.iter().all(SelectItem::is_header) {
        effects.bell = true;
        return;
    }
    ctx.overlays.push(Box::new(
        SelectList::new("Fleet", items, ctx.theme)
            .with_live_key(crate::attach::fleet::FLEET_LIVE_KEY),
    ));
}

/// ADR-0049: focus the next asking pane (window then DFS order, wrapping),
/// saving one return origin. No attention: bell, no origin.
pub(in crate::attach) fn next_attention(
    ActionCall {
        ctx,
        focused,
        panes,
        effects,
        ..
    }: ActionCall<'_, '_>,
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

/// Jump back to the saved origin, consuming it even when that pane is gone
/// (a bell, never a sticky origin that could resolve elsewhere).
pub(in crate::attach) fn return_from_attention(
    ActionCall { ctx, effects, .. }: ActionCall<'_, '_>,
) {
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

/// Focus the pane at (window, DFS ordinal), per-client; stale coordinates
/// bell rather than focus the wrong pane.
pub(in crate::attach) fn focus_pane(
    ActionCall {
        resolved,
        ctx,
        effects,
        ..
    }: ActionCall<'_, '_>,
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
        .groups
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
    focus_in_window(ctx, effects, win, target);
}

/// `agents-filter { scope?, host? }`: narrow the sidebar's Agents panel.
/// The driver owns the painter's filter and its file; a value it cannot
/// read bells and changes nothing.
pub(in crate::attach) fn agents_filter(
    ActionCall {
        resolved, effects, ..
    }: ActionCall<'_, '_>,
) {
    use crate::render::chrome::sidebar::{AgentScope, FilterRequest, HostFilter, Step};
    let scope = match str_arg(resolved, "scope").as_deref() {
        None => None,
        Some("next") => Some(Step::Next),
        Some(value) => {
            let Some(scope) = AgentScope::parse(value) else {
                tracing::warn!(scope = value, "agents-filter: unknown scope");
                effects.bell = true;
                return;
            };
            Some(Step::Set(scope))
        }
    };
    let host = str_arg(resolved, "host").map(|value| {
        if value == "next" {
            Step::Next
        } else {
            Step::Set(HostFilter::parse(&value))
        }
    });
    effects.agents_filter = Some(FilterRequest { scope, host });
}

/// ADR-0168: `toggle-project-group { project }` — collapse or expand one
/// project group in the sidebar. The driver owns the collapsed set.
pub(in crate::attach) fn toggle_project_group(
    ActionCall {
        resolved, effects, ..
    }: ActionCall<'_, '_>,
) {
    let Some(project) = str_arg(resolved, "project") else {
        tracing::warn!(args = ?resolved.args, "toggle-project-group needs a `project` arg");
        effects.bell = true;
        return;
    };
    effects.toggle_project = Some(project);
}
