//! The session this window shows, opened in place in the navigator.

use std::collections::HashSet;

use gpui::prelude::*;
use gpui::{AnyElement, ClickEvent, Context, IntoElement, MouseButton, SharedString, div, px};
use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use phux_client_runtime::control::{PaneDescriptor, Status};
use phux_protocol::ResourceId;
use phux_protocol::ResourceKind;
use phux_protocol::wire::frame::ResourceRecord;

use crate::settings::TabPosition;
use crate::workspace::layout;

use crate::app::color::from_hex;
use crate::app::desk::Desk;
use crate::app::fleet::Work;
use crate::app::skin::Look;
use crate::app::ui::list::{Emphasis, ICON, row_detail, row_label};
use crate::app::ui::menu::Anchor;
use crate::app::ui::{badge, ghost_icon, muted, row};
use crate::app::view::{ResourceDrag, tabs, tabs_at};

use super::rows::{pane_row, record_row, running};
use super::{host_word, session_slot};

/// The session this window shows: its row, then its tabs when they live
/// here, then the terminals no tab in this window holds.
pub(super) fn session_block(
    desk: &Desk,
    work: &Work,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let Some(link) = desk.link.as_ref() else {
        return div().into_any_element();
    };
    let (id, name) = (work.id, work.destination.name.clone());
    let placed: HashSet<&ResourceId> = link
        .workspace
        .tabs
        .iter()
        .flat_map(|tab| layout::placements(&tab.root))
        .filter_map(|placement| placement.terminal_id.as_ref())
        .collect();
    let background: Vec<PaneDescriptor> = link
        .connection
        .panes()
        .iter()
        .filter(|pane| pane.session_id == id && !placed.contains(&pane.terminal_id))
        .cloned()
        .collect();
    let session = link
        .connection
        .topology
        .as_ref()
        .and_then(|topology| topology.sessions.iter().find(|session| session.id == id));
    let destination = session.and_then(|session| session.key);
    let all_panes = link.connection.panes();
    let extra: Vec<ResourceRecord> = destination
        .and_then(|key| {
            desk.client().map(|client| {
                client
                    .organization()
                    .resources()
                    .iter()
                    .filter(|resource| {
                        resource.session == key
                            && resource.kind != ResourceKind::AgentSession
                            && running(resource)
                            && !all_panes.iter().any(|pane| pane.key == Some(resource.key))
                    })
                    .cloned()
                    .collect()
            })
        })
        .unwrap_or_default();
    let designated = link.connection.is_intake_session(id);
    let imported = session.is_some_and(|session| session.awaiting_organization) && !designated;
    let collapsed = desk.collapsed.contains(&name);
    let side_tabs = tabs_at(desk, &look.skin, TabPosition::Side);
    let terminals = all_panes
        .iter()
        .filter(|pane| pane.session_id == id)
        .count()
        + extra.len();
    let session_tip = format!(
        "{name} · {} · {}{terminals} terminals",
        desk.remote.as_deref().unwrap_or("This Mac"),
        if designated {
            "Intake, the home for unorganized work · "
        } else if imported {
            "Imported, awaiting organization · "
        } else {
            ""
        },
    );
    // The mark sits on the session while it is folded; open, the tab or
    // terminal that needs a person carries it.
    let attention = collapsed.then_some(work.attention).flatten();
    let online = link.connection.status == Status::Attached;
    let detail = match (host_word(desk, desk.remote.as_deref()), online) {
        (Some(host), true) => Some(host),
        (Some(host), false) => Some(format!("{host} · offline")),
        (None, false) => Some("offline".into()),
        (None, true) => None,
    };
    let toggle_name = name.clone();
    let (menu_name, context_name) = (name.clone(), name.clone());
    let header = row(SharedString::from(format!("session-{id}")), look)
        .debug_selector(move || format!("session-{id}"))
        .group("session")
        .child(session_slot(
            if collapsed { "▸" } else { "▾" },
            attention,
            look,
        ))
        .child(row_label(name.clone(), Emphasis::Current, look))
        .when(imported, |row| row.child(badge("imported", look)))
        .children(detail.map(|detail| row_detail(detail, look)))
        .child(
            div()
                .flex_shrink_0()
                .invisible()
                .group_hover("session", |style| style.visible())
                .when(desk.menu_open(&format!("session-{id}")), |slot| {
                    slot.visible()
                })
                .child(
                    ghost_icon(
                        format!("session-menu-{id}"),
                        IconName::Ellipsis,
                        "Session actions",
                    )
                    .on_click(cx.listener(
                        move |desk, event: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            desk.open_session_menu(
                                id,
                                &menu_name,
                                Anchor::At(event.position()),
                                window,
                                cx,
                            );
                        },
                    )),
                ),
        )
        .on_click(
            cx.listener(move |desk, _, _, cx| desk.toggle_session_collapsed(&toggle_name, cx)),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |desk, event: &gpui::MouseDownEvent, window, cx| {
                desk.open_session_menu(id, &context_name, Anchor::At(event.position), window, cx);
            }),
        )
        .when_some(destination, |row, key| {
            let destination_name = name.clone();
            let wash = from_hex(&look.chrome.accent_wash);
            row.drag_over::<ResourceDrag>(move |style, _, _, _| style.bg(wash))
                .on_drop(cx.listener(move |desk, drag: &ResourceDrag, window, cx| {
                    desk.move_resources_to(
                        drag.keys.clone(),
                        key,
                        destination_name.clone(),
                        window,
                        cx,
                    );
                }))
        })
        .tooltip(move |window, cx| Tooltip::new(session_tip.clone()).build(window, cx));
    let others = !background.is_empty() || !extra.is_empty();
    div()
        .flex()
        .flex_col()
        .gap(px(1.))
        .child(header)
        .when(!collapsed, |block| {
            block
                .when(side_tabs, |block| {
                    block.child(tabs::strip(desk, TabPosition::Side, look, cx))
                })
                .when(others && side_tabs, |block| {
                    block.child(
                        muted(look, "Not in a tab")
                            .pl(px(8. + ICON + 8.))
                            .pt(px(6.))
                            .pb(px(2.)),
                    )
                })
                .children(
                    background
                        .iter()
                        .map(|pane| pane_row(desk, pane, look, cx).into_any_element()),
                )
                .children(
                    extra
                        .iter()
                        .map(|resource| record_row(desk, resource, look, cx)),
                )
        })
        .into_any_element()
}
