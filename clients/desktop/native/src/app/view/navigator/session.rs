//! A session's disclosure is independent of the window's selected session.

use gpui::prelude::*;
use gpui::{AnyElement, ClickEvent, Context, IntoElement, MouseButton, SharedString, div, px};
use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;

use crate::app::color::from_hex;
use crate::app::desk::{Desk, display_title};
use crate::app::fleet::Work;
use crate::app::skin::Look;
use crate::app::ui::list::{Emphasis, row_detail, row_label};
use crate::app::ui::menu::Anchor;
use crate::app::ui::{badge, ghost_icon, row};
use crate::app::view::ResourceDrag;

use super::rows::{catalog_pane_row, pane_row, record_row};
use super::{host_word, session_slot};

/// A host-qualified persistent identity, shared by active and inactive rows.
/// A rename preserves disclosure; equal names on different hosts do not share it.
pub(super) fn disclosure_key(work: &Work) -> String {
    format!(
        "{:?}:{:?}:{}",
        work.destination.host,
        work.destination.authority,
        work.destination
            .key
            .map_or_else(|| format!("handle:{}", work.id), |key| key.to_string())
    )
}

pub(super) fn session_block(
    desk: &Desk,
    work: &Work,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let target = work.destination.clone();
    let current = desk.work_is_active(&target);
    let key = disclosure_key(work);
    let collapsed = desk.collapsed.contains(&key);
    let selector = format!("work-{}-{}", target.host_label(), work.id);
    let toggle_key = key.clone();
    let (detail, tip) = session_description(desk, work);
    let header = row(SharedString::from(selector.clone()), look)
        .debug_selector(move || selector.clone())
        .group("session")
        .when(current, |row| row.bg(look.active))
        .child(
            div()
                .id(SharedString::from(format!("disclose-{key}")))
                .debug_selector({ let target = target.clone(); let id = work.id; move || format!("disclose-work-{}-{id}", target.host_label()) })
                .role(gpui::Role::Button)
                .aria_label(if collapsed { "Expand session" } else { "Collapse session" })
                .cursor_pointer()
                .child(session_slot(if collapsed { "▸" } else { "▾" }, None, look))
                .on_click(cx.listener(move |desk, _, _, cx| {
                    cx.stop_propagation();
                    desk.toggle_session_collapsed(&toggle_key, cx);
                })),
        )
        .child(row_label(display_title(&target.name), if current { Emphasis::Current } else { Emphasis::Normal }, look))
        .when(work.imported, |row| row.child(badge("imported", look)))
        .child(row_detail(detail, look))
        .child(div().w(px(8.)).flex_shrink_0().child(crate::app::ui::status::state_dot(work.attention, None)))
        // Reserve the same actions slot on every row, selected or not.
        .child(session_actions(desk, work, current, look, cx))
        .when_some(work.destination.key.filter(|_| current), |row, destination| {
            let name = work.destination.name.clone();
            let wash = from_hex(&look.chrome.accent_wash);
            row.drag_over::<ResourceDrag>(move |style, _, _, _| style.bg(wash))
                .on_drop(cx.listener(move |desk, drag: &ResourceDrag, window, cx| {
                    desk.move_resources_to(drag.keys.clone(), destination, name.clone(), window, cx);
                }))
        })
        .when(current, |row| {
            let name = work.destination.name.clone();
            let id = work.id;
            row.on_mouse_down(MouseButton::Right, cx.listener(move |desk, event: &gpui::MouseDownEvent, window, cx| {
                desk.open_session_menu(id, &name, Anchor::At(event.position), window, cx);
            }))
        })
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(move |desk, _, _, cx| desk.navigate_work(target.clone(), cx)));
    div()
        .id(SharedString::from(format!("session-block-{key}")))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(1.))
        .child(header)
        .when(!collapsed, |block| {
            block
                .children(work.panes.iter().map(|pane| {
                    if current {
                        pane_row(desk, &pane.descriptor, look, cx).into_any_element()
                    } else {
                        catalog_pane_row(desk, work, pane, look, cx)
                    }
                }))
                .children(work.resources.iter().map(|resource| {
                    if current {
                        record_row(desk, resource, look, cx)
                    } else {
                        super::rows::catalog_record_row(work, resource, look, cx)
                    }
                }))
        })
        .into_any_element()
}

fn session_description(desk: &Desk, work: &Work) -> (String, String) {
    let mut words = vec![format!(
        "{} {}",
        work.terminals,
        if work.terminals == 1 { "pane" } else { "panes" }
    )];
    words.extend(host_word(desk, work.destination.host.as_deref()));
    if !work.online {
        words.push("offline".into());
    }
    let detail = words.join(" · ");
    let role = if work.intake {
        " · Intake, the home for unorganized work"
    } else if work.imported {
        " · Imported, awaiting organization"
    } else {
        ""
    };
    let tip = format!(
        "{} · {} · {detail}{role}",
        work.destination.name,
        work.destination.host.as_deref().unwrap_or("This Mac")
    );
    (detail, tip)
}

fn session_actions(
    desk: &Desk,
    work: &Work,
    current: bool,
    _: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let id = work.id;
    let name = work.destination.name.clone();
    div()
        .flex_shrink_0()
        .invisible()
        .when(current, |slot| {
            slot.group_hover("session", |style| style.visible())
        })
        .when(
            current && desk.menu_open(&format!("session-{id}")),
            |slot| slot.visible(),
        )
        .child(
            ghost_icon(
                format!("session-menu-{}-{id}", work.destination.host_label()),
                IconName::Ellipsis,
                "Session actions",
            )
            .on_click(cx.listener(move |desk, event: &ClickEvent, window, cx| {
                cx.stop_propagation();
                desk.open_session_menu(id, &name, Anchor::At(event.position()), window, cx);
            })),
        )
}
