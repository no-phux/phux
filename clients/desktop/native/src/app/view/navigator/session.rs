//! A session's disclosure is independent of the window's selected session.

use gpui::prelude::*;
use gpui::{AnyElement, ClickEvent, Context, IntoElement, MouseButton, SharedString, div, px};
use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;

use crate::app::color::from_hex;
use crate::app::desk::{Desk, display_title};
use crate::app::fleet::Work;
use crate::app::skin::Look;
use crate::app::ui::list::{Emphasis, ROW_GAP, row_detail, row_label};
use crate::app::ui::menu::Anchor;
use crate::app::ui::{ghost_icon, row};
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

const HEADER_GAP: f32 = 4.;
/// Title and caption lines, set close so they read as one entry.
const TITLE_LINE: f32 = 18.;
const DETAIL_LINE: f32 = 16.;

pub(super) fn session_block(
    desk: &Desk,
    work: &Work,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let target = work.destination.clone();
    let current = desk.work_is_active(&target);
    // Session actions target any session on the active connection, including
    // empty/inactive sessions. Selection is not mutation authority.
    let actionable = work.online && target.host == desk.remote;
    let key = disclosure_key(work);
    let collapsed = desk.collapsed.contains(&key);
    let selector = format!("work-{}-{}", target.host_label(), work.id);
    let toggle_key = key.clone();
    let (detail, tip) = session_description(desk, work);
    let header = row(SharedString::from(selector.clone()), look)
        .debug_selector(move || selector.clone())
        .group("session")
        .h(px(44.))
        .min_h(px(44.))
        .gap(px(HEADER_GAP))
        .when(current, |row| row.bg(look.active))
        .child(
            div()
                .id(SharedString::from(format!("disclose-{key}")))
                .debug_selector({ let target = target.clone(); let id = work.id; move || format!("disclose-work-{}-{id}", target.host_label()) })
                .role(gpui::Role::Button)
                .aria_label(if collapsed { "Expand session" } else { "Collapse session" })
                .cursor_pointer()
                // The whole row's height to hit; the caret sits on the title's line.
                .self_stretch()
                .flex()
                .items_center()
                .pb(px(DETAIL_LINE))
                // The title starts on its children's icon column.
                .mr(px(ROW_GAP - HEADER_GAP))
                .child(session_slot(collapsed, None, look))
                .on_click(cx.listener(move |desk, _, _, cx| {
                    cx.stop_propagation();
                    desk.toggle_session_collapsed(&toggle_key, cx);
                })),
        )
        // Metadata gets a stable second line, never the name's horizontal space.
        .child(div().flex_1().min_w_0().flex().flex_col().gap(px(1.))
            .child(row_label(display_title(&target.name), if current { Emphasis::Current } else { Emphasis::Normal }, look)
                .flex_none().w_full().line_height(px(TITLE_LINE))
                .debug_selector({ let target = target.clone(); let id = work.id; move || format!("session-label-{}-{id}", target.host_label()) }))
            .child(row_detail(detail, look).max_w_full().w_full().line_height(px(DETAIL_LINE))
                .debug_selector({ let target = target.clone(); let id = work.id; move || format!("session-detail-{}-{id}", target.host_label()) })))
        .child(div().w(px(8.)).flex_shrink_0().child(crate::app::ui::status::state_dot(work.attention, None)))
        // Reserve the same actions slot on every row, selected or not.
        .child(session_actions(work, actionable, cx))
        .when_some(work.destination.key.filter(|_| current), |row, destination| {
            let name = work.destination.name.clone();
            let wash = from_hex(&look.chrome.accent_wash);
            row.drag_over::<ResourceDrag>(move |style, _, _, _| style.bg(wash))
                .on_drop(cx.listener(move |desk, drag: &ResourceDrag, window, cx| {
                    desk.move_resources_to(drag.keys.clone(), destination, name.clone(), window, cx);
                }))
        })
        .when(actionable, |row| {
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
    if work.imported {
        words.push("imported".into());
    }
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

fn session_actions(work: &Work, actionable: bool, cx: &mut Context<Desk>) -> impl IntoElement {
    let id = work.id;
    let name = work.destination.name.clone();
    let selector = format!("session-actions-{}-{id}", work.destination.host_label());
    div()
        .debug_selector(move || selector.clone())
        .flex_shrink_0()
        .when(!actionable, |slot| slot.invisible())
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
