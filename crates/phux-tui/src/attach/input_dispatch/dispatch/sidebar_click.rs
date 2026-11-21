//! What a left press on the sidebar strip commits, in the same vocabulary
//! as a keybinding.

use super::super::args::{
    bare_action, focus_pane_action, select_window_action, switch_host_action,
};
use super::{SidebarHit, hit_test, switch_session_args};

/// Map a left press on the sidebar strip to the `ResolvedAction` it commits,
/// the same vocabulary as a keybinding.
pub(in crate::attach::input_dispatch) fn sidebar_click_action(
    strip: crate::layout::Rect,
    targets: &crate::render::chrome::sidebar::SidebarTargets,
    x: u16,
    y: u16,
) -> Option<phux_config::keybind::ResolvedAction> {
    let action = match hit_test(strip, targets.counts, x, y)? {
        SidebarHit::Window(i) => return select_window_action(i),
        SidebarHit::NeedsYou(j) => return sidebar_agent_action(targets.needs_you.get(j)?),
        SidebarHit::Roster(j) => {
            return Some(sidebar_session_action(targets.roster.get(j)?.as_ref()?));
        }
        SidebarHit::Project(j) => {
            let key = targets.projects.get(j)?.clone()?;
            return Some(crate::attach::resolved(
                "toggle-project-group",
                std::collections::BTreeMap::from([(
                    "project".to_owned(),
                    toml::Value::String(key),
                )]),
            ));
        }
        SidebarHit::Plugin(s, j) => {
            let (window, pane) = *targets.plugin.get(s)?.get(j)?;
            return focus_pane_action(window, pane);
        }
        SidebarHit::AgentFilter(chip) => return Some(agents_filter_action(chip)),
        SidebarHit::Sessions => "session-picker",
        SidebarHit::Fleet => "agent-fleet",
        SidebarHit::NewWindow => "new-window",
        SidebarHit::Collapse => "toggle-sidebar",
    };
    Some(bare_action(action))
}

/// Every row of `strip` a press would act on, top to bottom, as
/// `(strip-relative row, action)`; a target spanning rows keeps its first.
/// The `focus-sidebar` cursor walks these.
pub(in crate::attach) fn sidebar_rows(
    strip: crate::layout::Rect,
    targets: &crate::render::chrome::sidebar::SidebarTargets,
) -> crate::render::overlay::SidebarRows {
    // The middle column: never the rule, which sits at either edge.
    let x = strip.x + strip.w / 2;
    let mut rows: crate::render::overlay::SidebarRows = Vec::new();
    for row in 0..strip.h {
        let Some(action) = sidebar_click_action(strip, targets, x, strip.y + row) else {
            continue;
        };
        if rows.last().is_none_or(|(_, last)| *last != action) {
            rows.push((row, action));
        }
    }
    rows
}

/// A filter chip picks its scope; the `@` chip steps the machine.
fn agents_filter_action(
    chip: crate::render::chrome::sidebar::FilterChip,
) -> phux_config::keybind::ResolvedAction {
    use crate::render::chrome::sidebar::FilterChip;
    let (key, value) = match chip {
        FilterChip::Scope(scope) => ("scope", scope.name()),
        FilterChip::Host => ("host", "next"),
    };
    crate::attach::resolved(
        "agents-filter",
        std::collections::BTreeMap::from([(key.to_owned(), toml::Value::String(value.to_owned()))]),
    )
}

/// Agent targets distinguish client-local focus from a cross-session attach.
fn sidebar_agent_action(
    target: &crate::render::chrome::sidebar::SidebarTarget,
) -> Option<phux_config::keybind::ResolvedAction> {
    use crate::render::chrome::sidebar::SidebarTarget;
    let (id, name, window, pane, resource) = match target {
        SidebarTarget::Window(index) => return select_window_action(*index),
        SidebarTarget::Session {
            id,
            name,
            window,
            pane,
            resource,
        } => (id, name, window, pane, resource),
    };
    if let Some(resource) = resource
        && let Some(action) = crate::render::chrome::sidebar::satellite_open_action(resource)
    {
        return Some(action);
    }
    let mut args = switch_session_args(name.clone(), *id);
    if let Some(resource) = resource {
        args.insert(
            "resource".to_owned(),
            toml::Value::String(phux_client::selector::format_terminal_id(resource)),
        );
    }
    // Layout-backed rows may also name window/pane. Graph-only rows omit
    // both so a click cannot fabricate a TUI index.
    if let Some(window) = *window {
        args.insert(
            "window".to_owned(),
            toml::Value::Integer(i64::try_from(window).ok()?),
        );
    }
    if let Some(pane) = *pane {
        args.insert(
            "pane".to_owned(),
            toml::Value::Integer(i64::try_from(pane).ok()?),
        );
    }
    Some(crate::attach::resolved("switch-session", args))
}

/// Preserve host qualification even when two hosts use the same session name.
fn sidebar_session_action(
    target: &crate::render::chrome::sidebar::SessionRosterTarget,
) -> phux_config::keybind::ResolvedAction {
    if let Some(machine) = &target.switch_host {
        return switch_host_action(machine.clone(), target.name.clone());
    }
    let mut args = switch_session_args(target.name.clone(), target.id);
    if let Some(host) = &target.host {
        args.insert("host".to_owned(), toml::Value::String(host.clone()));
    }
    crate::attach::resolved("switch-session", args)
}
