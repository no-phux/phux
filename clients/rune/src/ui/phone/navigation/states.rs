//! The session list before it has rows: loading, empty and no match.
use crate::ui::tokens::Voice;
use rune::{El, col, el, row, text};

use super::{Desk, Look, card, grid};
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
        .px(grid::inset(p))
        .py(p.space.s2)
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
/// shape of what will appear, drawn from the same grid and metrics as the
/// rows that replace it so nothing moves when they do. The placeholders
/// hold still: a connection that never answers must not keep the phone
/// drawing.
fn loading(desk: &Desk, p: &Look) -> El {
    let s = &p.space;
    let section = |rows: usize| {
        col()
            .w_full()
            .gap(s.s1)
            .child(skeleton_heading(p))
            .child(card((0..rows).map(|_| skeleton_row(p)), p))
    };
    col()
        .key("phone-session-loading")
        .w_full()
        .gap(grid::block_gap(p))
        .label("Loading sessions")
        .child(
            text(format!("Reaching {}…", desk.machines.label()))
                .font(p.ty.caption)
                .color(p.roles.text.dim.paint())
                .px(grid::inset(p))
                .a11y_hidden(),
        )
        .child(section(2).a11y_hidden())
        .child(section(1).a11y_hidden())
}

fn bar(w: f32, h: f32, p: &Look) -> El {
    el().w(w)
        .h(h)
        .radius(p.radius.xs)
        .fill(p.roles.border.muted.paint())
}

/// `bar` centred on a text line `line` points tall.
fn on_line(bar: El, line: f32) -> El {
    el().h(line).items_center().child(bar)
}

/// A section heading's shape: its name over its summary.
fn skeleton_heading(p: &Look) -> El {
    let s = &p.space;
    col()
        .w_full()
        .min_h(s.touch_min)
        .px(grid::inset(p))
        .py(s.s1)
        .gap(s.s0_5)
        .child(on_line(bar(s.s16 + s.s12, s.s3, p), p.ty.lead.line_px()))
        .child(on_line(bar(s.s16, s.s2, p), p.ty.caption.line_px()))
}

/// A pane row's shape: its mark in the leading column, its title and
/// detail lines from the text column.
fn skeleton_row(p: &Look) -> El {
    let s = &p.space;
    let line = p.ty.body.line_px();
    let mark = grid::glyph(p) - s.s1;
    row()
        .w_full()
        .px(grid::inset(p))
        .py(s.s2 + s.s0_5)
        .gap(s.s2)
        .items_start()
        .child(grid::lead_slot(bar(mark, mark, p), line, p))
        .child(
            col()
                .grow(1.0)
                .gap(s.s0_5)
                .child(on_line(bar(s.s16 + s.s16, s.s2 + s.s0_5, p), line))
                .child(on_line(bar(s.s16, s.s2, p), p.ty.caption.line_px())),
        )
}
