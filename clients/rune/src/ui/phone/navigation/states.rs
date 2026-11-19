//! The session list before it has rows: loading, empty and no match.
use crate::ui::tokens::Voice;
use rune::{El, col, el, row, text};

use super::{Desk, Look};
use crate::ui::icons;
use crate::ui::tokens::Paint;

pub(super) fn empty_state(desk: &Desk, query: &str, p: &Look) -> El {
    if desk.connection.topology.is_none() {
        return loading(desk, p);
    }
    let query = query.trim();
    let (icon, title, detail) = if query.is_empty() {
        (
            &icons::PROMPT,
            "No sessions yet".to_owned(),
            "Start one with New session below.",
        )
    } else {
        (
            &icons::SEARCH,
            format!("No matches for “{query}”"),
            "Search looks through session, pane, agent and folder names.",
        )
    };
    let s = &p.space;
    col()
        .key("phone-session-empty")
        .w_full()
        .items_center()
        .px(s.s6)
        .py(s.s10)
        .gap(s.s2)
        .child(
            el().size(s.s12, s.s12)
                .radius(p.radius.full)
                .center()
                .fill(p.roles.layer.raised.paint())
                .mb(s.s2)
                .child(
                    rune::icon(icon, p.icon.size.lg)
                        .color(p.roles.text.dim.paint())
                        .a11y_hidden(),
                ),
        )
        .child(
            text(title)
                .font(p.ty.lead.semibold())
                .color(p.roles.text.text.paint())
                .role("heading")
                .align(rune::Align::Center)
                .wrap(),
        )
        .child(
            text(detail)
                .font(p.ty.body)
                .color(p.roles.text.dim.paint())
                .align(rune::Align::Center)
                .wrap(),
        )
}

/// Before the first inventory arrives: where it is coming from, over the
/// shape of what will appear. The placeholders hold still: a connection
/// that never answers must not keep the phone drawing.
fn loading(desk: &Desk, p: &Look) -> El {
    let s = &p.space;
    let bar = |w: f32, h: f32| {
        el().w(w)
            .h(h)
            .radius(p.radius.full)
            // The page's own ground, cut into the raised card: visible in
            // both modes without a new colour.
            .fill(p.roles.layer.ground.paint())
    };
    let card = |rows: usize| {
        let mut card = col()
            .w_full()
            .padding(s.s4)
            .gap(s.s4)
            .radius(p.radius.xl)
            .continuous_corners()
            .fill(p.roles.layer.raised.paint())
            .child(
                col()
                    .gap(s.s2)
                    .child(bar(s.s16 + s.s12, s.s3))
                    .child(bar(s.s16, s.s2)),
            );
        for _ in 0..rows {
            card = card.child(
                row()
                    .gap(s.s3)
                    .items_center()
                    .child(bar(s.s3, s.s3))
                    .child(bar(s.s16 + s.s16, s.s2 + s.s0_5)),
            );
        }
        card
    };
    col()
        .key("phone-session-loading")
        .w_full()
        .gap(s.s3)
        .label("Loading sessions")
        .child(
            text(format!("Reaching {}…", desk.machines.label()))
                .font(p.ty.caption)
                .color(p.roles.text.dim.paint())
                .px(s.s1)
                .a11y_hidden(),
        )
        .child(card(2).a11y_hidden())
        .child(card(1).a11y_hidden())
}
