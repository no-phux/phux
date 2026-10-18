//! A pane's floating controls capsule and its file-drop hint.

use gpui::prelude::*;
use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, Window, div, px,
    relative,
};
use gpui_kit::assets::IconName;

use crate::workspace::layout::Placement;

use crate::app::color::from_hex;
use crate::app::desk::Desk;
use crate::app::pane_identity::PaneOrigin;
use crate::app::skin::{Look, Surface, Surfaced};
use crate::app::ui::menu::Anchor;
use crate::app::ui::status::harness_mark;
use crate::app::ui::{ghost_button, ghost_icon, muted, notice_card};

/// The floating capsule a pane shows at its top edge: what runs there, then
/// split, zoom and close.
pub(super) fn pane_controls(
    desk: &Desk,
    placement: &Placement,
    origin: PaneOrigin,
    zoomed: bool,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let title = desk.pane_title(placement.terminal_id.as_ref());
    let agent = placement
        .terminal_id
        .as_ref()
        .and_then(|id| desk.agent(id))
        .cloned();
    let id = placement.id.clone();
    let focus_id = placement.id.clone();
    let zoom = on_pane(id.clone(), "zoom");
    let zoom_label = if zoomed { "Restore" } else { "Zoom" };
    div()
        .id(SharedString::from(format!("pane-controls-{id}")))
        .absolute()
        .top(px(7.))
        .right(px(8.))
        .max_w(relative(0.9))
        .h(px(28.))
        .pl(px(10.))
        .pr(px(3.))
        .flex()
        .items_center()
        .gap(px(6.))
        .surface(look, Surface::Capsule)
        .occlude()
        .text_xs()
        .on_hover(super::card::track_hover(
            id.clone(),
            |desk| &mut desk.controls_hover,
            cx,
        ))
        .children(
            origin.harness().map(|harness| {
                harness_mark(harness, agent.as_ref().map(|info| info.state), 14., look)
            }),
        )
        .child(
            div()
                .flex_shrink_0()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(look.ink)
                .child(match agent.as_ref().map(|info| (info, info.word())) {
                    Some((info, "")) => info.name.clone(),
                    Some((info, word)) => format!("{} · {word}", info.name),
                    None => title,
                }),
        )
        .child(div().w(px(2.)).flex_shrink_0())
        .when(desk.conversation_eligible(&id), |row| {
            let conversation_id = id.clone();
            // The wider label sizes the slot, so the toggle never moves the
            // capsule under the pointer that clicked it.
            row.child(
                div()
                    .relative()
                    .flex_shrink_0()
                    .child(
                        ghost_button(format!("pane-conversation-width-{id}"), "Conversation")
                            .invisible(),
                    )
                    .child(
                        div().absolute().inset_0().child(
                            ghost_button(
                                format!("pane-conversation-{id}"),
                                if desk.conversation_visible(&id) {
                                    "Terminal"
                                } else {
                                    "Conversation"
                                },
                            )
                            .w_full()
                            .on_click(cx.listener(
                                move |desk, _, window, cx| {
                                    cx.stop_propagation();
                                    desk.toggle_conversation(&conversation_id, window, cx);
                                },
                            )),
                        ),
                    ),
            )
        })
        .child(
            ghost_icon(
                format!("zoom-{id}"),
                if zoomed {
                    IconName::Minimize
                } else {
                    IconName::Maximize
                },
                zoom_label,
            )
            .on_click(cx.listener(move |desk, _, window, cx| {
                cx.stop_propagation();
                zoom(desk, window, cx);
            })),
        )
        .child({
            let placement = id.clone();
            let selector = id.clone();
            div()
                .relative()
                .debug_selector(move || format!("pane-actions-{selector}"))
                .child(desk.anchors.probe(format!("pane-actions-{id}")))
                .child(
                    ghost_icon(
                        format!("pane-more-{id}"),
                        IconName::Ellipsis,
                        "Pane actions",
                    )
                    .on_click(cx.listener(
                        move |desk, event: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            let anchor = desk
                                .anchors
                                .get(&format!("pane-actions-{placement}"))
                                .map_or(Anchor::At(event.position()), Anchor::Below);
                            desk.open_pane_menu(&placement, anchor, window, cx);
                        },
                    )),
                )
        })
        .on_click(cx.listener(move |desk, event: &ClickEvent, window, cx| {
            if let Some(tab) = desk.active_tab_id() {
                desk.focus_placement(&tab, &focus_id, window, cx);
            }
            if event.click_count() >= 2 {
                desk.execute("zoom", window, cx);
            }
        }))
}

/// Shown over the pane a file drag from the OS is above.
pub(super) fn drop_hint(look: &Look, remote: bool) -> impl IntoElement {
    let accent = from_hex(&look.chrome.theme.accent);
    let (title, detail) = if remote {
        (
            "Drop to upload",
            "Sent to that machine, then its path is pasted",
        )
    } else {
        ("Drop to attach", "Paths paste here, ready for your agent")
    };
    div()
        .absolute()
        .inset_0()
        .rounded(px(look.skin.shape.radius))
        .bg(accent.opacity(0.1))
        .border_2()
        .border_color(accent.opacity(0.8))
        .flex()
        .items_center()
        .justify_center()
        .child(
            notice_card(look, 0.92)
                .px(px(16.))
                .py(px(10.))
                .rounded(px(look.skin.shape.card_radius - 2.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(2.))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(look.ink)
                        .child(title),
                )
                .child(muted(look, detail)),
        )
}

/// Focus this pane, then run `command` on it.
fn on_pane(
    placement: String,
    command: &'static str,
) -> impl Fn(&mut Desk, &mut Window, &mut Context<Desk>) + 'static {
    move |desk, window, cx| {
        if let Some(tab) = desk.active_tab_id() {
            desk.focus_placement(&tab, &placement, window, cx);
        }
        desk.execute(command, window, cx);
    }
}
