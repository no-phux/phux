//! Exceptions where they apply: the titlebar's connection and startup chips,
//! a scrolled-back pane's Follow live pill, and the notification peek, the
//! newest alert, briefly, over the workspace.

use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt, AnyElement, Context, Hsla, IntoElement, Window, div, ease_out_quint,
    px, rems,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;

use super::super::color::from_hex;
use super::super::desk::{Desk, Modal, Tone};
use super::super::notifications::{Kind, Notification};
use super::super::skin::{Look, Surface, Surfaced};
use super::super::ui::{agent_swatch, dot, ghost_icon, muted};

/// The notification peek's width while the pane area has room for it.
const PEEK_WIDTH: f32 = 340.;
/// The peek's distance from the pane cards' right and bottom edges.
const PEEK_INSET: f32 = 12.;
/// The peek's padding around its content.
const PEEK_PAD: f32 = 12.;
/// The Follow live pill's distance from its pane's foot, and its height.
const FOLLOW_INSET: f32 = 12.;
const FOLLOW_HEIGHT: f32 = 28.;

/// Exceptions only, in the titlebar: an unhealthy connection and a terminal
/// still starting. A pane scrolled back says so on itself ([`follow_live`]);
/// agents that need a person are marked on their tabs and sessions.
pub(super) fn status_chips(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> Option<AnyElement> {
    let colors = &look.chrome;
    let (label, tone) = desk.connection_label();
    let pending = desk
        .link
        .as_ref()
        .map_or(0, |link| link.workspace.pending());
    if tone == Tone::Good && pending == 0 {
        return None;
    }
    let tone_color = from_hex(match tone {
        Tone::Good => &colors.theme.success,
        Tone::Busy => &colors.theme.warning,
        Tone::Bad => &colors.theme.danger,
    });
    let unhealthy = desk.unhealthy();
    let chip = |id: &'static str| {
        div()
            .id(id)
            .h(px(24.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded_full()
            .text_xs()
    };
    let moves = super::chrome_moves(desk, cx);
    let chips = div()
        .debug_selector(|| "status-chips".into())
        .flex()
        .items_center()
        .gap(px(2.))
        .mr(px(4.))
        .when(tone != Tone::Good, |chips| {
            chips.child(
                chip("status-connection")
                    .bg(tone_color.opacity(0.12))
                    .text_color(tone_color)
                    .child(dot(6., tone_color))
                    .child(label)
                    .when(unhealthy, |status| {
                        status
                            .cursor_pointer()
                            .hover(move |style| style.bg(tone_color.opacity(0.2)))
                            .on_click(cx.listener(|desk, _, window, cx| {
                                desk.execute("reconnect", window, cx)
                            }))
                    }),
            )
        })
        .when(pending > 0, |chips| {
            chips.child(
                chip("status-pending")
                    .text_color(from_hex(&colors.muted))
                    .child("Starting terminal…"),
            )
        });
    Some(enter(chips, "status-chips", look, moves, |chips, shown| {
        chips.opacity(shown)
    }))
}

/// A pane scrolled away from live output: one pill, centred at its foot, back
/// to the tail.
pub(super) fn follow_live(look: &Look, moves: bool, cx: &mut Context<Desk>) -> AnyElement {
    let colors = &look.chrome;
    let accent = from_hex(&colors.theme.accent);
    let pill = div()
        .absolute()
        .left_0()
        .right_0()
        .bottom(px(FOLLOW_INSET))
        .flex()
        .justify_center()
        .child(
            div()
                .id("follow-live")
                .debug_selector(|| "follow-live".into())
                // Clicks stop here; the wheel still scrolls the pane under it.
                .block_mouse_except_scroll()
                .h(px(FOLLOW_HEIGHT))
                .pl(px(10.))
                .pr(px(12.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded_full()
                .surface(look, Surface::Capsule)
                .text_xs()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(look.ink)
                .cursor_pointer()
                .hover(move |style| style.bg(accent.opacity(0.18)))
                .child(
                    Icon::new(IconName::ArrowDown)
                        .size(px(12.))
                        .text_color(accent),
                )
                .child("Follow live")
                .on_click(cx.listener(|desk, _, window, cx| {
                    cx.stop_propagation();
                    desk.execute("follow", window, cx)
                })),
        );
    enter(pill, "follow-live", look, moves, |pill, shown| {
        pill.opacity(shown).mb(px(-6. * (1. - shown)))
    })
}

/// The chrome's entrance: `animate` over the skin's menu span, eased out,
/// once; still when the chrome does not move.
fn enter<E: IntoElement + 'static>(
    element: E,
    id: impl Into<gpui::ElementId>,
    look: &Look,
    moves: bool,
    animate: impl Fn(E, f32) -> E + 'static,
) -> AnyElement {
    if !moves {
        return element.into_any_element();
    }
    element
        .with_animation(
            id,
            Animation::new(look.skin.motion.menu_enter()).with_easing(ease_out_quint()),
            animate,
        )
        .into_any_element()
}

/// The colour a notification wears: its agent's state, or its kind.
pub(crate) fn notification_edge(entry: &Notification, look: &Look) -> Hsla {
    let theme = &look.chrome.theme;
    from_hex(match entry.kind {
        Kind::Error => &theme.danger,
        Kind::Agent => match entry.agent_state {
            Some(state) => agent_swatch(state),
            None => &theme.warning,
        },
        Kind::Approval => &theme.warning,
        Kind::Info | Kind::Refusal | Kind::Command => &theme.accent,
    })
}

/// A notification's title over up to `lines` of its body; `strong` sets the
/// title in medium weight.
pub(crate) fn notification_text(
    entry: &Notification,
    look: &Look,
    strong: bool,
    lines: usize,
) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .text_sm()
                .when(strong, |title| title.font_weight(gpui::FontWeight::MEDIUM))
                .text_color(look.ink)
                .child(entry.title.clone()),
        )
        .children(
            entry
                .body
                .clone()
                .map(|body| muted(look, body).when(lines > 0, |body| body.line_clamp(lines))),
        )
}

/// The newest agent or error entry, briefly, over the workspace. Everything
/// else waits behind the bell. It is a notification row lifted onto the
/// popover material: the kind's dot on the title line, the body under it.
pub(super) fn peek_card(
    desk: &Desk,
    look: &Look,
    window: &Window,
    cx: &mut Context<Desk>,
) -> Option<impl IntoElement> {
    let colors = &look.chrome;
    if desk.modal == Modal::Notifications {
        return None;
    }
    let entry = desk.feed.peek()?;
    let id = entry.id;
    let more = desk.feed.unread().count().saturating_sub(1);
    let gutter = look.skin.shape.gutter;
    // Over a scrolled-back pane, the peek clears its Follow live pill.
    let moves = super::chrome_moves(desk, cx);
    let lift = if desk.follows_live() {
        FOLLOW_HEIGHT + PEEK_INSET
    } else {
        0.
    };
    // Spans the pane area, held PEEK_INSET in from its cards' edges, so the
    // card narrows with the area instead of escaping it.
    let area = div()
        .absolute()
        .left(px(super::panes::lead_inset(desk, look) + PEEK_INSET))
        .right(px(gutter + PEEK_INSET))
        .bottom(px(super::panes::foot(desk, look) + PEEK_INSET + lift))
        .flex()
        .justify_end();
    let title_line =
        super::super::overlays::line_height(window, rems(0.875).to_pixels(window.rem_size()));
    let card = div()
        .id(("notification-peek", id as usize))
        .group("notification-peek")
        .relative()
        .w(px(PEEK_WIDTH))
        .min_w_0()
        .surface(look, Surface::Popover)
        .bg(from_hex(&colors.raised).opacity(0.96))
        .when(!look.popover_shadow.is_empty(), |card| card.shadow_lg())
        .p(px(PEEK_PAD))
        .pr(px(PEEK_PAD - 4.))
        .occlude()
        .cursor_pointer()
        .flex()
        .items_start()
        .gap(px(10.))
        .child(
            div()
                .flex_shrink_0()
                .h(title_line)
                .flex()
                .items_center()
                .child(dot(7., notification_edge(entry, look))),
        )
        .child(
            notification_text(entry, look, true, 3).when(more > 0, |text| {
                text.child(
                    div()
                        .pt(px(2.))
                        .text_xs()
                        .text_color(from_hex(&colors.subtext))
                        .child(format!(
                            "+{}",
                            super::super::desk::counted(more, "unread alert")
                        )),
                )
            }),
        )
        .child(
            // Shown with the pointer on the card, so a glance reads only words.
            div()
                .flex_shrink_0()
                .opacity(0.)
                .group_hover("notification-peek", |style| style.opacity(1.))
                .child(
                    ghost_icon(
                        ("notification-peek-dismiss", id as usize),
                        IconName::Close,
                        "Dismiss",
                    )
                    .on_click(cx.listener(move |desk, _, _, cx| {
                        cx.stop_propagation();
                        desk.feed.read(id);
                        cx.notify();
                    })),
                ),
        )
        .on_click(cx.listener(move |desk, _, window, cx| {
            desk.open_notification(id, window, cx);
        }));
    Some(area.child(enter(
        card,
        ("notification-peek-enter", id as usize),
        look,
        moves,
        |card, shown| card.opacity(shown).top(px(8. * (1. - shown))),
    )))
}
