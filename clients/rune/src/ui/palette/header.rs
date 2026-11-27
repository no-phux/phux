//! The palette's field row: the mode's glyph on the marks' column, the
//! query on the titles', a chip for a sub-mode; on a desktop the count of
//! matches, on a phone a search bar with Cancel.

use crate::ui::tokens::Voice;
use rune::{Cursor, El, el, row, text, text_input};

use super::super::icons;
use super::super::overlay::EXTENTS;
use super::super::tokens::{Paint, Tokens, Type};
use super::Grid;
use crate::desk::Desk;
use crate::palette::Mode;

/// What the field says it searches, by mode.
fn prompt(mode: Mode, phone: bool) -> &'static str {
    match mode {
        Mode::All if phone => "Search",
        Mode::All => "Jump to a terminal, session, or action…",
        Mode::Mirror(source) => source.prompt(),
        Mode::Shortcuts => "Filter keyboard shortcuts…",
        Mode::Essentials => "The keys worth learning first; pick one to try it…",
        Mode::Playground => "Open a playground scene…",
        Mode::Sessions => "Switch session, or name a new one…",
        Mode::Themes => "Preview a theme…",
        Mode::Paths => "Find a file or folder…",
        Mode::Screens => "Filter displays and windows…",
        Mode::Popup => "A command to run in a popup…",
    }
}

/// A sub-mode's name, shown as a chip before the query.
const fn mode_name(mode: Mode) -> Option<&'static str> {
    Some(match mode {
        Mode::All => return None,
        Mode::Mirror(_) => "Mirror",
        Mode::Shortcuts => "Shortcuts",
        Mode::Essentials => "Essentials",
        Mode::Playground => "Playground",
        Mode::Sessions => "Sessions",
        Mode::Themes => "Themes",
        Mode::Paths => "Paths",
        Mode::Screens => "Screens",
        Mode::Popup => "Popup",
    })
}

/// What Return does, and the glyph the field leads with.
pub(super) fn verb_and_glyph(mode: Mode) -> (&'static str, &'static rune::Icon) {
    match mode {
        Mode::Paths => ("insert", &icons::FOLDER),
        Mode::Themes => ("keep", &icons::PALETTE),
        Mode::Sessions => ("switch", &icons::WINDOW),
        _ => ("open", &icons::SEARCH),
    }
}

fn field(desk: &Desk, t: &Tokens, font: rune::TextStyle, phone: bool) -> El {
    let r = &t.roles;
    text_input(&desk.palette_query, font)
        .key("palette-query")
        .label("Command search")
        // The modal opens on its field, ahead of the list's picked row.
        .autofocus()
        .single_line()
        .grow(1.0)
        // A narrow window truncates the hint rather than overflowing.
        .min_w(0.0)
        .color(r.text.text.paint())
        .placeholder(prompt(desk.palette_mode, phone), r.text.faint.paint())
        .on_edit(|d: &mut Desk| &mut d.palette_query)
        .on_input_key(|d: &mut Desk, event, tx| d.palette_key(event, tx))
        .on_input_change(|d: &mut Desk, _| d.palette_edited())
        .on_submit(|d: &mut Desk, tx| d.palette_run(None, tx))
}

/// A sub-mode's chip: where the query is scoped.
fn mode_chip(name: &'static str, ty: &Type, t: &Tokens) -> El {
    let s = &t.space;
    row()
        .key("palette-mode")
        .h(s.s5)
        .px(s.s2)
        .items_center()
        .radius(t.radius.full)
        .fill(t.roles.wash.current.paint())
        .label(format!("{name} mode"))
        .child(
            text(name)
                .font(ty.caption.semibold())
                .color(t.roles.text.text.paint())
                .a11y_hidden(),
        )
}

/// The desktop's field row: the mode's glyph in the marks' column, the
/// query on the titles', a chip for a sub-mode and the count of matches.
pub(super) fn desktop_header(desk: &Desk, t: &Tokens, g: &Grid, count: usize) -> El {
    let (_, glyph) = verb_and_glyph(desk.palette_mode);
    let typed = !desk.palette_query.text.trim().is_empty();
    let counted = match count {
        1 => "1 result".to_owned(),
        n => format!("{n} results"),
    };
    row()
        .key("palette-header")
        .w_full()
        .h(EXTENTS.palette_header_h)
        .px(g.lead())
        .gap(g.gap)
        .items_center()
        .child(
            el().key("palette-glyph").w(g.slot).center().child(
                rune::icon(glyph, t.icon.size.md)
                    .color(t.roles.accent.accent_text.paint())
                    .a11y_hidden(),
            ),
        )
        .children(mode_name(desk.palette_mode).map(|name| mode_chip(name, &g.ty, t)))
        .child(field(desk, t, g.ty.lead, false))
        .children((typed && count > 0).then(|| {
            text(counted)
                .key("palette-count")
                .font(g.ty.caption.figures())
                .color(t.roles.text.faint.paint())
        }))
}

/// The search bar's type: the phone's, stopped at the grid's text cap as
/// iOS stops a bar's, so the field keeps room for its hint beside Cancel
/// at the largest text.
fn bar_type(t: &Tokens) -> Type {
    crate::ui::tokens::phone::ty(bar_scale(t))
}

fn bar_scale(t: &Tokens) -> f32 {
    t.type_scale.min(crate::desk::accessibility::GRID_CAP)
}

/// The phone's search bar, as the Sessions page draws its own: the
/// magnifier in the marks' column and the query on the titles', with
/// Cancel beside it.
pub(super) fn phone_header(desk: &Desk, t: &Tokens, g: &Grid) -> El {
    let s = &t.space;
    let margin = crate::ui::tokens::phone::page_margin();
    let dim = t.roles.text.dim.paint();
    let edge = if t.increased_contrast {
        t.roles.border.control
    } else {
        t.roles.border.muted
    };
    let search = row()
        .key("palette-search")
        .grow(1.0)
        .min_w(0.0)
        .pl(g.lead() - margin)
        .pr(s.s2)
        .gap(g.gap)
        .items_center()
        .radius(t.radius.lg)
        .continuous_corners()
        .fill(t.roles.layer.raised.paint())
        .border(s.hairline, edge.paint())
        .child(
            el().key("palette-glyph")
                .w(g.slot)
                .center()
                .child(
                    rune::icon(&icons::SEARCH, t.icon.grid * bar_scale(t))
                        .color(dim)
                        .a11y_hidden(),
                ),
        )
        // No mode chip: at large text it would take the field's room, and
        // the hint already says what is searched.
        // The lead role: an iOS field under 16 points zooms the page.
        .child(
            field(desk, t, bar_type(t).lead, true)
                .min_h(s.touch_min)
                .py(s.s2),
        );
    let cancel = t
        .kit
        .interactive(
            row()
                .key("palette-cancel")
                .role("button")
                .label("Cancel")
                .min_h(s.touch_min)
                .min_w(s.touch_min)
                .px(s.s1)
                .center(),
        )
        .cursor(Cursor::Pointer)
        // A press leaves the field its keyboard until the page closes;
        // Escape is Cancel's key.
        .preserve_focus_on_press(true)
        .on_press_tx(|d: &mut Desk, tx| {
            d.close_palette();
            tx.blur();
        })
        .child(
            text("Cancel")
                .font(bar_type(t).body)
                .color(t.roles.accent.accent_text.paint())
                .a11y_hidden(),
        );
    row()
        .key("palette-header")
        .w_full()
        .px(margin)
        .py(s.s2)
        .gap(s.s2)
        .items_center()
        .child(search)
        .child(cancel)
}
