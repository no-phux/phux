//! The status line, which speaks only for exceptions, and the notification
//! peek: the newest alert, briefly, over the workspace.

use gpui::prelude::*;
use gpui::{Context, Hsla, IntoElement, div, px};
use gpui_kit::assets::IconName;

use super::super::color::from_hex;
use super::super::desk::{Desk, Modal, Tone};
use super::super::notifications::{Kind, Notification};
use super::super::skin::Look;
use super::super::ui::{agent_swatch, dot, ghost_icon, muted, notice_card};
use super::chip_radius;

/// The notification peek's width while the pane area has room for it.
const PEEK_WIDTH: f32 = 340.;
/// The peek's distance from the pane cards' right and bottom edges.
const PEEK_INSET: f32 = 12.;
/// The width of the peek's colour stripe.
const PEEK_STRIPE: f32 = 3.;

/// Exceptions only: an unhealthy connection, a terminal still starting, and
/// a pane scrolled away from live output. Agents that need a person are
/// marked on their tabs and sessions, not counted here.
pub(super) fn status_bar(
    desk: &Desk,
    height: f32,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let colors = &look.chrome;
    let radius = chip_radius(&look.skin);
    let chip = |id: &'static str| div().id(id).rounded(px(radius));
    let (label, tone) = desk.connection_label();
    let unhealthy = desk.unhealthy();
    let scrolled = desk.scrolled_back();
    let pending = desk
        .link
        .as_ref()
        .map_or(0, |link| link.workspace.pending());
    let tone_color = from_hex(match tone {
        Tone::Good => &colors.theme.success,
        Tone::Busy => &colors.theme.warning,
        Tone::Bad => &colors.theme.danger,
    });
    let hover = look.hover;
    div()
        .h(px(height))
        .w_full()
        .flex_shrink_0()
        .overflow_hidden()
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(12.))
        .text_xs()
        .text_color(from_hex(&colors.muted))
        .when(tone != Tone::Good, |bar| {
            bar.child(
                chip("status-connection")
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(4.))
                    .child(dot(7., tone_color))
                    .child(div().text_color(tone_color).child(label))
                    .when(unhealthy, |status| {
                        status
                            .cursor_pointer()
                            .hover(move |style| style.bg(hover))
                            .on_click(cx.listener(|desk, _, window, cx| {
                                desk.execute("reconnect", window, cx)
                            }))
                    }),
            )
        })
        .when(pending > 0, |bar| bar.child("starting terminal…"))
        .child(div().flex_grow(1.))
        .when(scrolled, |bar| {
            bar.child(
                chip("status-scrolled")
                    .px(px(6.))
                    .bg(from_hex(&colors.accent_wash))
                    .text_color(from_hex(&colors.theme.accent))
                    .cursor_pointer()
                    .child("scrolled back · follow live")
                    .on_click(
                        cx.listener(|desk, _, window, cx| desk.execute("follow", window, cx)),
                    ),
            )
        })
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
                .map(|body| muted(look, body).line_clamp(lines)),
        )
}

/// The newest agent or error entry, briefly, over the workspace. Everything
/// else waits behind the bell.
pub(super) fn peek_card(
    desk: &Desk,
    look: &Look,
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
    // Spans the pane area, held PEEK_INSET in from its cards' edges, so the
    // card narrows with the area instead of escaping it.
    let area = div()
        .absolute()
        .left(px(gutter + PEEK_INSET))
        .right(px(gutter + PEEK_INSET))
        .bottom(px(PEEK_INSET))
        .flex()
        .justify_end();
    let card = notice_card(look, 0.94)
        .id(("notification-peek", id as usize))
        .relative()
        .overflow_hidden()
        .w(px(PEEK_WIDTH))
        .min_w_0()
        .p(px(10.))
        .pl(px(12. + PEEK_STRIPE))
        .occlude()
        .cursor_pointer()
        .flex()
        .gap(px(8.))
        .child(stripe(notification_edge(entry, look), look))
        .child(
            notification_text(entry, look, true, 3).when(more > 0, |text| {
                text.child(
                    div()
                        .text_xs()
                        .text_color(from_hex(&colors.subtext))
                        .child(format!("+{more} unread alerts")),
                )
            }),
        )
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
        )
        .on_click(cx.listener(move |desk, _, window, cx| {
            desk.open_notification(id, window, cx);
        }));
    Some(area.child(card))
}

/// A notice's colour down its left side, inside the hairline. Clips are
/// rectangular, so the stripe is a rounded border that follows the corners.
fn stripe(color: Hsla, look: &Look) -> impl IntoElement {
    div()
        .absolute()
        .inset_0()
        .rounded(px((look.skin.shape.radius - 1.).max(0.)))
        .border_l(px(PEEK_STRIPE))
        .border_color(color)
}
