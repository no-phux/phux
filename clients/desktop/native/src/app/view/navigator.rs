//! The sidebar, the one navigator: every session across this Mac and saved
//! machines, grouped by project, by machine, or not at all. A session is one
//! line, folded until its caret opens it; empty sessions sink below the ones
//! holding work. Disclosure and host visibility never follow selection.
//! Sidebar tabs occupy a separate, fixed-height shelf so they cannot displace
//! the tree. Where work runs is a word on its row only where its name alone
//! is ambiguous; what needs a person is a mark on its row, not a list.
//!
//! Groupings are views of the same catalog ([`Desk::work_catalog`]); they
//! never add, hide or reorder work within a session.

use std::collections::HashSet;

use gpui::prelude::*;
use gpui::{
    AnimationExt, AnyElement, Context, IntoElement, MouseButton, SharedString, Window, div, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::Disableable;
use gpui_kit::component::button::Button;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable};

use crate::settings::{SidebarGrouping, TabPosition};

use super::super::color::from_hex;
use super::super::desk::{self, Desk};
use super::super::fleet::Work;
use super::super::menus;
use super::super::skin::Look;
use super::super::ui::list::{Emphasis, row_detail, row_label};
use super::super::ui::{dot, ghost_button, ghost_icon, muted, row, row_icon, section_label};
use super::tabs;

mod rows;
mod session;

use session::session_block;

const FOOTER_HEIGHT: f32 = 44.;
const TAB_SHELF_HEIGHT: f32 = 160.;
/// The list's inset from the sidebar's edges; rows highlight within it.
pub(super) const LIST_INSET: f32 = 8.;

/// Whether the prefs show the sidebar in this window.
pub(super) fn wanted(desk: &Desk) -> bool {
    desk.prefs.sidebar_visible && desk.role != desk::WindowRole::Quick
}

pub(super) fn build(desk: &mut Desk, _: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    let look = desk.look();
    let integrated = super::integrated_titlebar(desk, &look.skin);
    div()
        .w(px(desk.prefs.sidebar_width as f32))
        .h_full()
        .flex_shrink_0()
        .relative()
        .flex()
        .flex_col()
        .when(integrated, |sidebar| {
            sidebar.child(super::titlebar::sidebar_header(desk, &look, cx))
        })
        .child(host_filter(desk, &look, cx))
        .child(
            div().flex_1().min_h_0().relative().flex().flex_col()
            .child(list(desk, &look, cx))
            // Reserved scroll padding keeps selection controls from moving
            // the catalog; anchoring here follows any footer size.
            .child(div().absolute().left_0().right_0().bottom_0()
                .child(organization_bar(desk, &look, cx))),
        )
        .when(desk.tab_position() == TabPosition::Side, |sidebar| {
            sidebar.child(tab_shelf(desk, &look, cx))
        })
        .when(integrated, |sidebar| {
            sidebar.child(super::titlebar::sidebar_tools(desk, &look, cx))
        })
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
        // Work without a project sinks below the projects. Projects meet by
        // name, so a linked project and a repository of that name are one.
        // Within a group, empty sessions sink below the ones holding work, so
        // a session moves when it gains its first terminal or loses its last.
        SidebarGrouping::Project => work.sort_by(|a, b| {
            let name = |work: &Work| work.project.as_ref().map(|(_, name)| name.clone());
            (
                a.project.is_none(),
                name(a),
                a.terminals == 0,
                &a.destination.name,
                &a.destination.host,
            )
                .cmp(&(
                    b.project.is_none(),
                    name(b),
                    b.terminals == 0,
                    &b.destination.name,
                    &b.destination.host,
                ))
        }),
        SidebarGrouping::Machine => work.sort_by(|a, b| {
            (
                a.destination.host.is_some(),
                &a.destination.host,
                a.terminals == 0,
                &a.destination.name,
            )
                .cmp(&(
                    b.destination.host.is_some(),
                    &b.destination.host,
                    b.terminals == 0,
                    &b.destination.name,
                ))
        }),
        SidebarGrouping::None => work.sort_by(|a, b| {
            (a.terminals == 0, &a.destination.name, &a.destination.host).cmp(&(
                b.terminals == 0,
                &b.destination.name,
                &b.destination.host,
            ))
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
        .h(px(FOOTER_HEIGHT))
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
                .h(px(24.))
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
                        .size(px(14.))
                        .text_color(from_hex(&look.chrome.muted)),
                )
                .child(muted(look, "Search").flex_1().text_size(px(13.)))
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

/// The catalog under the host filter: what the list can show.
fn visible_work(desk: &Desk) -> Vec<Work> {
    desk.work_catalog()
        .into_iter()
        .filter(|work| {
            desk.prefs
                .sidebar_shows_host(work.destination.host.as_deref())
        })
        .collect()
}

/// The disclosure keys of every listed session that has rows to show.
pub(crate) fn foldable_sessions(desk: &Desk) -> Vec<String> {
    visible_work(desk)
        .iter()
        .filter(|work| session::foldable(work))
        .map(session::disclosure_key)
        .collect()
}

/// Session names listed more than once, on different machines.
fn repeated_names(work: &[Work]) -> HashSet<String> {
    let mut seen = HashSet::new();
    work.iter()
        .map(|work| work.destination.name.clone())
        .filter(|name| !seen.insert(name.clone()))
        .collect()
}

fn list(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let work = visible_work(desk);
    let repeated = repeated_names(&work);
    let groups = group(work, desk.prefs.sidebar_grouping);
    let mut rows = Vec::new();
    for group in groups {
        if let Some(label) = &group.label {
            // Machines group by their saved name and read as what they resolve to.
            let label = match desk.prefs.sidebar_grouping {
                SidebarGrouping::Machine => super::super::machine_name::display(label),
                _ => label.clone(),
            };
            rows.push(section_label(&label, look).into_any_element());
        }
        for work in group.work {
            let ambiguous = repeated.contains(&work.destination.name);
            rows.push(session_block(desk, &work, ambiguous, look, cx));
        }
    }
    let empty = rows.is_empty();
    div()
        .id("sidebar-list")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(LIST_INSET))
        .pt(px(4.))
        .pb(px(48.))
        .flex()
        .flex_col()
        .gap(px(1.))
        .when(empty, |list| {
            list.child(
                muted(look, empty_words(desk))
                    .px(px(8.))
                    .py(px(6.))
                    .text_sm(),
            )
        })
        .children(rows)
        .children(unavailable(desk, look, cx))
}

/// What an empty list says: why there is nothing, not a filter that isn't set.
fn empty_words(desk: &Desk) -> &'static str {
    use phux_client_runtime::control::Status;
    match desk.link.as_ref().map(|link| link.connection.status) {
        Some(Status::Attached) => "No sessions match this view.",
        Some(Status::Idle | Status::Connecting | Status::Negotiated) => "Connecting…",
        _ => "Not connected. Sessions show here once phux is reachable.",
    }
}

/// The machine a row names, only where its name alone is ambiguous: the same
/// session name on two machines. A machine heading already says it.
fn host_word(desk: &Desk, host: Option<&str>, ambiguous: bool) -> Option<String> {
    (ambiguous && desk.prefs.sidebar_grouping != SidebarGrouping::Machine)
        .then(|| super::super::machine_name::label(host))
}

/// A session's caret. It turns, on the chrome spring, rather than swap.
fn session_caret(key: &str, collapsed: bool, moves: bool, look: &Look) -> gpui::Div {
    let turn = if collapsed {
        0.
    } else {
        std::f32::consts::FRAC_PI_2
    };
    row_icon(
        Icon::new(IconName::ChevronRight)
            .size(px(12.))
            .text_color(from_hex(&look.chrome.muted))
            .with_spring(
                SharedString::from(format!("disclose-caret-{key}")),
                super::chrome_spring(moves, turn),
                |caret, turn| caret.rotate(gpui::radians(turn)),
            ),
    )
}

/// A stable filter control: clicking work never changes these exclusions.
fn host_filter(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let filtered = !desk.prefs.sidebar_hidden_hosts.is_empty();
    div()
        .h(px(32.))
        .flex_shrink_0()
        .px(px(8.))
        .flex()
        .items_center()
        .child(
            div()
                .relative()
                .debug_selector(|| "sidebar-host-filter".into())
                .child(desk.anchors.probe(menus::HOST_FILTER))
                .child(
                    ghost_button(
                        "sidebar-host-filter",
                        if filtered {
                            "Machines · filtered"
                        } else {
                            "All machines"
                        },
                    )
                    .toggled(desk.menu_open(menus::HOST_FILTER))
                    .on_click(
                        cx.listener(|desk, _, window, cx| {
                            desk.open_sidebar_host_filter(window, cx)
                        }),
                    ),
                ),
        )
        .when(filtered, |bar| {
            bar.child(
                div()
                    .debug_selector(|| "sidebar-host-filter-reset".into())
                    .child(ghost_button("sidebar-host-filter-reset", "Reset").on_click(
                        cx.listener(|desk, _, _, cx| {
                            desk.update_prefs(|prefs| prefs.sidebar_hidden_hosts.clear(), cx)
                        }),
                    )),
            )
        })
        .text_color(from_hex(&look.chrome.muted))
}

/// Workspace tabs have a fixed home, not a variable-height insertion into
/// whichever session happens to be selected. Each region scrolls separately.
fn tab_shelf(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    div()
        .id("sidebar-tab-shelf")
        .debug_selector(|| "sidebar-tab-shelf".into())
        .h(px(TAB_SHELF_HEIGHT))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .child(tab_list(desk, look, cx))
}

/// The labelled, scrolling side tabs that fill their shelf or rail.
pub(super) fn tab_list(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .px(px(8.))
        .child(section_label("Tabs", look).flex_shrink_0())
        .child(
            div()
                .id("sidebar-tab-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(tabs::strip(desk, TabPosition::Side, look, cx)),
        )
}

/// Machines whose inventory could not be read, each a retry.
fn unavailable(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> Vec<AnyElement> {
    desk.fleet
        .failures()
        .into_iter()
        .filter(|(host, _)| desk.prefs.sidebar_shows_host(host.as_deref()))
        .map(|(host, error)| {
            let name = super::super::machine_name::label(host.as_deref());
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
        .bg(look.sheet)
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
