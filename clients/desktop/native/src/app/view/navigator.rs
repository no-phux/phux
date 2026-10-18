//! The sidebar, the one navigator: every session across this Mac and saved
//! machines, grouped by project, by machine, or not at all. The session this
//! window shows opens in place: its tabs when they live in the sidebar, then
//! any of its terminals no tab holds. Where work runs is a word on its row,
//! not a chip; what needs a person is a mark on its row, not a list.
//!
//! Groupings are views of the same catalog ([`Desk::work_catalog`]); they
//! never add, hide or reorder work within a session.

use gpui::prelude::*;
use gpui::{AnyElement, Context, IntoElement, MouseButton, SharedString, Window, div, px};
use gpui_kit::assets::IconName;
use gpui_kit::base::Disableable;
use gpui_kit::component::button::Button;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable};

use crate::connection::AgentState;
use crate::settings::{SidebarGrouping, TabPosition};

use super::super::color::from_hex;
use super::super::desk::{self, Desk};
use super::super::fleet::Work;
use super::super::menus;
use super::super::skin::Look;
use super::super::ui::list::{Emphasis, row_detail, row_label};
use super::super::ui::status::state_dot;
use super::super::ui::{dot, ghost_button, ghost_icon, muted, row, row_icon, section_label};
use super::{tabs, tabs_at};

mod rows;
mod session;

use session::session_block;

/// Whether the prefs show the sidebar in this window.
pub(super) fn wanted(desk: &Desk) -> bool {
    desk.prefs.sidebar_visible && desk.role != desk::WindowRole::Quick
}

pub(super) fn build(desk: &mut Desk, _: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    let look = desk.look();
    div()
        .w(px(desk.prefs.sidebar_width as f32))
        .h_full()
        .flex_shrink_0()
        .relative()
        .flex()
        .flex_col()
        .child(list(desk, &look, cx))
        .child(footer(desk, &look, cx))
        .child(resize(desk, &look, cx))
        .into_any_element()
}

/// One heading and its sessions. A lone group goes unlabelled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Group {
    pub label: Option<String>,
    pub work: Vec<Work>,
}

/// The catalog under `grouping`'s headings. Groups keep the catalog's order
/// within them; a list with one group drops its heading.
pub(crate) fn group(mut work: Vec<Work>, grouping: SidebarGrouping) -> Vec<Group> {
    let key = |work: &Work| -> Option<String> {
        match grouping {
            SidebarGrouping::Project => Some(
                work.project
                    .as_ref()
                    .map_or_else(|| "Other".to_owned(), |(_, name)| name.clone()),
            ),
            SidebarGrouping::Machine => Some(
                work.destination
                    .host
                    .clone()
                    .unwrap_or_else(|| "This Mac".into()),
            ),
            SidebarGrouping::None => None,
        }
    };
    match grouping {
        // Work without a project sinks below the projects.
        SidebarGrouping::Project => work.sort_by(|a, b| {
            (
                a.project.is_none(),
                &a.project,
                &a.destination.name,
                &a.destination.host,
            )
                .cmp(&(
                    b.project.is_none(),
                    &b.project,
                    &b.destination.name,
                    &b.destination.host,
                ))
        }),
        SidebarGrouping::Machine => work.sort_by(|a, b| {
            (
                a.destination.host.is_some(),
                &a.destination.host,
                &a.destination.name,
            )
                .cmp(&(
                    b.destination.host.is_some(),
                    &b.destination.host,
                    &b.destination.name,
                ))
        }),
        SidebarGrouping::None => work.sort_by(|a, b| {
            (&a.destination.name, &a.destination.host)
                .cmp(&(&b.destination.name, &b.destination.host))
        }),
    }
    let mut groups: Vec<Group> = Vec::new();
    for item in work {
        let label = key(&item);
        match groups.last_mut() {
            Some(last) if last.label == label => last.work.push(item),
            _ => groups.push(Group {
                label,
                work: vec![item],
            }),
        }
    }
    if groups.len() == 1 {
        groups[0].label = None;
    }
    groups
}

/// The navigator's foot: search, its view, and the machines. The list above
/// carries no chrome of its own.
fn footer(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let ink = look.ink;
    let chord = desk.shown_chord("goto");
    div()
        .debug_selector(|| "sidebar-footer".into())
        .flex_shrink_0()
        .h(px(44.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(2.))
        .child(
            div()
                .id("sidebar-filter")
                .debug_selector(|| "sidebar-filter".into())
                .flex_1()
                .min_w_0()
                .h(px(28.))
                .px(px(10.))
                .mr(px(4.))
                .flex()
                .items_center()
                .gap(px(7.))
                .rounded_full()
                .cursor_pointer()
                .bg(ink.opacity(0.05))
                .hover(move |style| style.bg(ink.opacity(0.08)))
                .role(gpui::Role::Button)
                .aria_label("Find work, terminals and tabs")
                .child(
                    Icon::new(IconName::Search)
                        .size(px(13.))
                        .text_color(from_hex(&look.chrome.muted)),
                )
                .child(muted(look, "Filter").flex_1().text_sm())
                .children(chord.map(|chord| muted(look, chord).flex_shrink_0()))
                .on_click(cx.listener(|desk, _, window, cx| desk.execute("goto", window, cx))),
        )
        .child(
            div()
                .relative()
                .debug_selector(|| "sidebar-view".into())
                .child(desk.anchors.probe(menus::VIEW))
                .child(
                    ghost_icon("sidebar-view", IconName::ListTree, "Sidebar view")
                        .small()
                        .toggled(desk.menu_open(menus::VIEW))
                        .on_click(
                            cx.listener(|desk, _, window, cx| desk.open_view_menu(window, cx)),
                        ),
                ),
        )
        .child(
            ghost_icon("sidebar-machines", IconName::Server, "Machines…")
                .small()
                .on_click(cx.listener(|desk, _, window, cx| desk.execute("machines", window, cx))),
        )
}

fn list(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let groups = group(desk.work_catalog(), desk.prefs.sidebar_grouping);
    let mut rows = Vec::new();
    let mut shown = false;
    for group in groups {
        if let Some(label) = &group.label {
            rows.push(section_label(label, look).into_any_element());
        }
        for work in group.work {
            rows.push(if desk.work_is_active(&work.destination) {
                shown = true;
                session_block(desk, &work, look, cx)
            } else {
                work_row(desk, work, look, cx)
            });
        }
    }
    // Before the catalog lists this window's session (still connecting, or
    // offline), its tabs still need a home here.
    if !shown && desk.link.is_some() {
        rows.insert(0, unlisted_session(desk, look, cx));
    }
    let empty = rows.is_empty();
    div()
        .id("sidebar-list")
        .flex_grow(1.)
        .overflow_y_scroll()
        .px(px(8.))
        .pt(px(4.))
        .pb(px(8.))
        .flex()
        .flex_col()
        .gap(px(1.))
        .child(organization_bar(desk, look, cx))
        .when(empty, |list| {
            list.child(
                muted(look, "No sessions yet.")
                    .px(px(8.))
                    .py(px(6.))
                    .text_sm(),
            )
        })
        .children(rows)
        .children(unavailable(desk, look, cx))
}

/// The machine a row names: nothing on this Mac or under a machine heading.
fn host_word(desk: &Desk, host: Option<&str>) -> Option<String> {
    (desk.prefs.sidebar_grouping != SidebarGrouping::Machine)
        .then(|| host.map(str::to_owned))
        .flatten()
}

/// A session's leading slot: its caret, or a lit dot while something in it
/// needs a person.
fn session_slot(caret: &'static str, attention: Option<AgentState>, look: &Look) -> gpui::Div {
    row_icon(match attention {
        Some(state) => state_dot(Some(state), None),
        None => div()
            .text_xs()
            .text_color(from_hex(&look.chrome.faint))
            .child(caret),
    })
}

/// Another session: one row that opens it here.
fn work_row(desk: &Desk, work: Work, look: &Look, cx: &mut Context<Desk>) -> AnyElement {
    let target = work.destination.clone();
    let machine = target.host_label().to_owned();
    let tip = format!(
        "{} · {} · {} terminals{}",
        target.name,
        if machine == "Local" {
            "This Mac"
        } else {
            &machine
        },
        work.terminals,
        if work.online { "" } else { " · offline" }
    );
    let selector = format!("work-{machine}-{}", work.id);
    let detail = match (host_word(desk, target.host.as_deref()), work.online) {
        (Some(host), true) => Some(host),
        (Some(host), false) => Some(format!("{host} · offline")),
        (None, false) => Some("offline".into()),
        (None, true) => None,
    };
    row(SharedString::from(selector.clone()), look)
        .debug_selector(move || selector.clone())
        .child(session_slot("›", work.attention, look))
        .child(row_label(
            target.name.clone(),
            if work.online {
                Emphasis::Normal
            } else {
                Emphasis::Quiet
            },
            look,
        ))
        .children(detail.map(|detail| row_detail(detail, look)))
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(move |desk, _, _, cx| desk.navigate_work(target.clone(), cx)))
        .into_any_element()
}
/// This window's session while the catalog cannot list it: its name, how
/// its connection stands, and its tabs when they live here.
fn unlisted_session(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> AnyElement {
    let name = if desk.session.is_empty() {
        "phux".to_owned()
    } else {
        desk.session.clone()
    };
    let collapsed = desk.collapsed.contains(&name);
    let side_tabs = tabs_at(desk, &look.skin, TabPosition::Side);
    let (state, _) = desk.connection_label();
    let toggle_name = name.clone();
    div()
        .flex()
        .flex_col()
        .gap(px(1.))
        .child(
            row("session-current", look)
                .debug_selector(|| "session-current".into())
                .child(session_slot(if collapsed { "▸" } else { "▾" }, None, look))
                .child(row_label(name, Emphasis::Current, look))
                .child(row_detail(state, look))
                .on_click(cx.listener(move |desk, _, _, cx| {
                    desk.toggle_session_collapsed(&toggle_name, cx)
                })),
        )
        .when(side_tabs && !collapsed, |block| {
            block.child(tabs::strip(desk, TabPosition::Side, look, cx))
        })
        .into_any_element()
}

/// Machines whose inventory could not be read, each a retry.
fn unavailable(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> Vec<AnyElement> {
    desk.fleet
        .failures()
        .into_iter()
        .map(|(host, error)| {
            let name = host.as_deref().unwrap_or("This Mac").to_owned();
            let selector = format!("unavailable-{}", host.as_deref().unwrap_or("Local"));
            let tip = format!("{error} · Click to retry");
            row(SharedString::from(selector.clone()), look)
                .debug_selector(move || selector.clone())
                .child(row_icon(dot(
                    6.,
                    from_hex(&look.chrome.theme.danger).opacity(0.7),
                )))
                .child(row_label(
                    format!("{name} unavailable"),
                    Emphasis::Quiet,
                    look,
                ))
                .child(row_detail("Retry", look))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .on_click(cx.listener(|desk, _, _, cx| desk.retry_fleet(cx)))
                .into_any_element()
        })
        .collect()
}

fn organization_bar(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> AnyElement {
    let enabled = desk
        .link
        .as_ref()
        .is_some_and(|link| link.connection.organization());
    let count = desk.organization.selected.len();
    let busy = desk.organization.busy();
    if !enabled || (count == 0 && !busy && desk.organization.undo.is_none()) {
        return div().into_any_element();
    }
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .px(px(8.))
        .py(px(6.))
        .child(
            muted(
                look,
                if busy {
                    "Moving…".to_owned()
                } else if count > 0 {
                    format!("{count} selected")
                } else {
                    String::new()
                },
            )
            .flex_1(),
        )
        .when(count > 0, |row| {
            row.child(
                Button::new("move-selected-work")
                    .label("Move…")
                    .small()
                    .disabled(busy)
                    .on_click(cx.listener(|desk, _, window, cx| {
                        desk.execute("move-resources", window, cx)
                    })),
            )
            .child(
                ghost_button("clear-work-selection", "Clear")
                    .disabled(busy)
                    .on_click(cx.listener(|desk, _, _, cx| {
                        desk.organization.selected.clear();
                        cx.notify();
                    })),
            )
        })
        .when(desk.organization.undo.is_some(), |row| {
            row.child(
                Button::new("undo-organization")
                    .label("Undo")
                    .small()
                    .disabled(busy)
                    .on_click(cx.listener(|desk, _, window, cx| {
                        desk.execute("undo-organization", window, cx)
                    })),
            )
        })
        .into_any_element()
}

fn resize(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let active = desk.dragging_sidebar();
    let accent = from_hex(&look.chrome.theme.accent);
    div()
        .id("sidebar-resize")
        .absolute()
        .right(px(-2.))
        .top(px(0.))
        .w(px(5.))
        .h_full()
        .cursor_col_resize()
        .when(active, |handle| handle.bg(accent))
        .hover(move |style| style.bg(accent.opacity(0.5)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|desk, _, _, cx| desk.begin_sidebar_drag(cx)),
        )
}

#[cfg(test)]
mod tests;
