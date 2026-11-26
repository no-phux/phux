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
    if !query.trim().is_empty() {
        return no_matches(p);
    }
    let s = &p.space;
    col()
        .key("phone-session-empty")
        .w_full()
        .items_center()
        .px(s.s6)
        .py(s.s10)
        .gap(s.s2)
        .child(
            el().size(s.s12, s.s12).center().mb(s.s2).child(
                rune::icon(&icons::PROMPT, p.icon.size.lg)
                    .color(p.roles.text.dim.paint())
                    .a11y_hidden(),
            ),
        )
        .child(
            text("No sessions yet")
                .font(p.ty.lead.semibold())
                .color(p.roles.text.text.paint())
                .role("heading")
                .align(rune::Align::Center)
                .wrap_anywhere(),
        )
        .child(
            text("Start one with New session below.")
                .font(p.ty.body)
                .color(p.roles.text.dim.paint())
                .align(rune::Align::Center)
                .wrap(),
        )
}

/// The query already lives in the editor. Keep recovery feedback near it,
/// without repeating a potentially unbounded name as a second large heading.
fn no_matches(p: &Look) -> El {
    col()
        .key("phone-session-empty")
        .w_full()
        .px(p.space.s3)
        .py(p.space.s4)
        .gap(p.space.s2)
        .child(
            text("No matches")
                .font(p.ty.lead.semibold())
                .color(p.roles.text.text.paint())
                .role("heading")
                .wrap(),
        )
        .child(
            text("Try another name or clear your search.")
                .font(p.ty.body)
                .color(p.roles.text.dim.paint())
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
            .radius(p.radius.xs)
            .fill(p.roles.border.muted.paint())
    };
    let section = |rows: usize| {
        let mut section = col().w_full().gap(s.s4).child(
            col()
                .gap(s.s2)
                .child(bar(s.s16 + s.s12, s.s3))
                .child(bar(s.s16, s.s2)),
        );
        for _ in 0..rows {
            section = section.child(
                row()
                    .min_h(super::row_h(p))
                    .pl(s.s4)
                    .gap(s.s2)
                    .items_center()
                    .child(bar(s.s3, s.s3))
                    .child(bar(s.s16 + s.s16, s.s2 + s.s0_5)),
            );
        }
        section
    };
    col()
        .key("phone-session-loading")
        .w_full()
        .gap(s.s6)
        .label("Loading sessions")
        .child(
            text(format!("Reaching {}…", desk.machines.label()))
                .font(p.ty.caption)
                .color(p.roles.text.dim.paint())
                .px(s.s1)
                .a11y_hidden(),
        )
        .child(section(2).a11y_hidden())
        .child(section(1).a11y_hidden())
}
