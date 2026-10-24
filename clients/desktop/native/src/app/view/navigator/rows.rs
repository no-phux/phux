//! Rows under a session: its terminals, and a record that is not a terminal.

use std::time::{SystemTime, UNIX_EPOCH};

use gpui::prelude::*;
use gpui::px;
use gpui::{
    AnyElement, AnyView, App, ClickEvent, Context, IntoElement, SharedString, WeakEntity, Window,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::tooltip::Tooltip;
use phux_client_runtime::control::PaneDescriptor;
use phux_protocol::ResourceId;
use phux_protocol::ResourceKind;
use phux_protocol::wire::frame::{ExecState, ResourceRecord};

use crate::app::color::from_hex;
use crate::app::desk::Desk;
use crate::app::pane_identity::PaneOrigin;
use crate::app::skin::Look;
use crate::app::ui::list::{Emphasis, child_row, row_detail, row_label};
use crate::app::ui::status::{harness_mark, state_dot};
use crate::app::ui::{ago, dot, row_icon};
use crate::app::view::{ResourceDrag, drag_label};
use crate::connection::AgentInfo;

fn exec_state(resource: &ResourceRecord) -> Option<ExecState> {
    resource
        .activation
        .as_ref()
        .map(|activation| activation.state)
}

pub(super) fn record_row(
    desk: &Desk,
    resource: &ResourceRecord,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let key = resource.key;
    let selected = desk.organization.selected.contains(&key);
    let kind = match resource.kind {
        ResourceKind::Terminal => "Terminal",
        _ => "Item",
    };
    let label = record_label(resource);
    let state = match exec_state(resource) {
        Some(ExecState::Starting) => "Starting",
        Some(ExecState::Running) => "Running",
        Some(ExecState::Stopping) => "Stopping",
        Some(ExecState::Completed) => "Exited",
        Some(ExecState::Failed) => "Failed to start",
        Some(ExecState::Lost | ExecState::Abandoned) => "Lost",
        None => "Not running",
    };
    let tooltip_label = label.clone();
    let keys = desk.organization.drag_keys(key);
    let drag = ResourceDrag {
        label: drag_label(&keys, &label),
        keys,
    };
    child_row(SharedString::from(format!("resource-{key}")), look)
        .when(selected, |row| row.bg(look.active))
        .child(row_icon(dot(5., from_hex(&look.chrome.muted))))
        .child(row_label(label, Emphasis::Quiet, look))
        .when(state != "Running", |row| row.child(row_detail(state, look)))
        .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
        .on_click(cx.listener(move |desk, _, _, cx| desk.toggle_resource_selection(key, cx)))
        .tooltip(move |window, cx| {
            Tooltip::new(format!("{tooltip_label}\n{kind} · {state}\n{key}")).build(window, cx)
        })
        .into_any_element()
}

/// A terminal of this session. Clicking shows its tab, opening one if none
/// holds it; Command-click selects it for organization.
pub(super) fn pane_row(
    desk: &Desk,
    pane: &PaneDescriptor,
    title: String,
    suffix: Option<String>,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let terminal = pane.terminal_id.clone();
    let key = pane.key;
    let selected = key.is_some_and(|key| desk.organization.selected.contains(&key));
    let drag = key.map(|key| {
        let keys = desk.organization.drag_keys(key);
        let label = drag_label(&keys, &title);
        ResourceDrag { keys, label }
    });
    let agent = desk.agent(&terminal);
    let attention = agent.filter(|info| info.needs_you()).map(|info| info.state);
    let glyph = pane_mark(desk.terminal_origin(&terminal), agent, look);
    let tooltip_terminal = terminal.clone();
    let handle = cx.weak_entity();
    let selector = SharedString::from(format!("pane-{terminal}"));
    child_row(selector.clone(), look)
        .debug_selector(move || selector.to_string())
        .when(selected, |row| row.bg(look.active))
        .child(row_icon(glyph))
        .child(
            row_label(
                title,
                if attention.is_some() {
                    Emphasis::Normal
                } else {
                    Emphasis::Quiet
                },
                look,
            )
            .debug_selector({
                let terminal = pane.terminal_id.clone();
                move || format!("pane-label-{terminal}")
            }),
        )
        .children(suffix.map(|suffix| {
            let terminal = pane.terminal_id.clone();
            row_detail(suffix, look).debug_selector(move || format!("pane-suffix-{terminal}"))
        }))
        .when_some(drag, |row, drag| {
            row.on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
        })
        .on_click(cx.listener(move |desk, event: &ClickEvent, window, cx| {
            if event.modifiers().platform
                && let Some(key) = key
            {
                desk.toggle_resource_selection(key, cx);
            } else {
                desk.reveal_terminal(&terminal);
            }
            desk.after_command(window, cx);
        }))
        .tooltip(move |window, cx| pane_tooltip(&tooltip_terminal, &handle, window, cx))
}

/// Inventory rows are visible before a session is selected. Opening one
/// navigates first and defers the reveal until that authority is attached.
pub(super) fn catalog_pane_row(
    work: &crate::app::fleet::Work,
    pane: &crate::app::fleet::WorkPane,
    suffix: Option<String>,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let destination = work.destination.clone();
    let terminal = pane.descriptor.terminal_id.clone();
    let title = pane.label.clone();
    let selector = format!("catalog-pane-{}-{terminal}", destination.host_label());
    child_row(SharedString::from(format!("pane-{terminal}")), look)
        .debug_selector(move || selector.clone())
        .child(row_icon(pane_mark(pane.origin, pane.agent.as_ref(), look)))
        .child(row_label(title, Emphasis::Quiet, look))
        .children(suffix.map(|suffix| {
            let selector = format!(
                "catalog-pane-suffix-{}-{terminal}",
                destination.host_label()
            );
            row_detail(suffix, look).debug_selector(move || selector)
        }))
        .on_click(cx.listener(move |desk, _, _, cx| {
            desk.open_work_terminal(destination.clone(), terminal.clone(), cx);
        }))
        .into_any_element()
}

pub(super) fn catalog_record_row(
    work: &crate::app::fleet::Work,
    resource: &ResourceRecord,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let destination = work.destination.clone();
    child_row(
        SharedString::from(format!("resource-{}", resource.key)),
        look,
    )
    .child(row_icon(dot(5., from_hex(&look.chrome.faint))))
    .child(row_label(record_label(resource), Emphasis::Quiet, look))
    .on_click(cx.listener(move |desk, _, _, cx| desk.navigate_work(destination.clone(), cx)))
    .into_any_element()
}

/// Identity never depends on selection. Keep the harness visible while its
/// corner pip reports state; a badge without a known harness still gets a dot.
fn pane_mark(origin: PaneOrigin, agent: Option<&AgentInfo>, look: &Look) -> AnyElement {
    let state = agent.map(|info| info.state);
    if let Some(harness) = origin.harness() {
        return harness_mark(harness, state, 12., look).into_any_element();
    }
    if let Some(info) = agent {
        return state_dot(info.needs_you().then_some(info.state), state).into_any_element();
    }
    terminal_glyph(look).into_any_element()
}

/// A terminal nobody's agent drives: a terminal, not a bullet.
fn terminal_glyph(look: &Look) -> Icon {
    Icon::new(IconName::SquareTerminal)
        .size(px(12.))
        .text_color(from_hex(&look.chrome.faint))
}

fn record_label(resource: &ResourceRecord) -> String {
    resource
        .name
        .as_deref()
        .map(crate::app::desk::display_title)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| match resource.kind {
            ResourceKind::Terminal => "Terminal".into(),
            _ => "Item".into(),
        })
}

/// Detail stays available on hover without repeating it in every row.
fn pane_tooltip(
    terminal: &ResourceId,
    handle: &WeakEntity<Desk>,
    window: &mut Window,
    cx: &mut App,
) -> AnyView {
    let details = if let Some(desk) = handle.upgrade() {
        let desk = desk.read(cx);
        let mut details = desk.pane_title(Some(terminal));
        if let Some(agent) = desk.agent(terminal) {
            details.push_str(&format!("\n{} · {}", agent.name, agent.word()));
            if let Some(provider) = &agent.kind {
                details.push_str(&format!(" · {provider}"));
            }
            if agent.urgent() {
                details.push_str(" · attention");
            }
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|since| since.as_millis() as u64)
                .unwrap_or(0);
            details.push_str(&format!(" · {}", ago(agent.changed_at_ms, now)));
        } else if let Some(harness) = desk.terminal_origin(terminal).harness() {
            details.push_str(&format!("\n{}", harness.label()));
        }
        if let Some(pane) = desk
            .link
            .as_ref()
            .and_then(|link| link.connection.pane(terminal))
            && let Some(cwd) = &pane.cwd
        {
            details.push_str(&format!("\n{cwd}"));
        }
        details.push_str(&format!("\n{terminal}"));
        details
    } else {
        terminal.to_string()
    };
    Tooltip::new(details).build(window, cx)
}
