//! Find on a phone: docked right above the key bar, under the thumb, never
//! over the terminal's text. Its field and its count share one capsule;
//! the older and newer matches and Done are whole touch targets beside it.
//! It rises from behind the key bar as a sheet does and leaves the same
//! way. The desktop's toolbar keeps its place over the pane.
use rune::{El, row, text, text_input};

use super::{Look, glyph, icons, round, touchable};
use crate::desk::Desk;
use crate::ui::tokens::{Paint, Voice};

pub(super) fn view(desk: &Desk, p: &Look) -> Option<El> {
    let shown = desk.connection.shown.as_ref()?;
    let find = (desk.connection.find.as_ref()).filter(|f| f.terminal == shown.resource)?;
    let s = &p.space;
    let r = &p.roles;
    let query = &desk.connection.find_query;
    let miss = find.index.is_none() && !query.text.is_empty();
    let font = p.ty.lead;
    let line = font.line_px();
    let field = text_input(query, font)
        .key("find-query")
        .label("Find in terminal")
        .single_line()
        .grow(1.0)
        .min_w(0.0)
        // The field centres its line by its padding, as the palette's does.
        .py(((s.touch_min - line) / 2.0).max(s.s2))
        .color(r.text.text.paint())
        .placeholder("Find", r.text.faint.paint())
        .invalid(miss)
        .on_edit(|d: &mut Desk| &mut d.connection.find_query)
        .on_input_key(|d: &mut Desk, event, tx| d.find_key(event, tx))
        .on_input_change(|d: &mut Desk, _| d.run_search());
    let count = if miss { r.status.failed } else { r.text.dim };
    let capsule = row()
        .key("phone-find-field")
        .grow(1.0)
        .min_w(0.0)
        .min_h(s.touch_min)
        .pl(s.s3)
        .pr(s.s3)
        .gap(s.s2)
        .items_center()
        .radius(p.radius.full)
        .continuous_corners()
        .fill(r.layer.raised.paint())
        .border(
            s.hairline,
            if miss { r.status.failed } else { r.border.base }.paint(),
        )
        .child(glyph(&icons::SEARCH, p.icon.size.md, r.text.dim.paint()).a11y_hidden())
        .child(field)
        .child(
            text(find.status(&query.text))
                .key("phone-find-count")
                .role("status")
                .font(p.ty.caption.figures())
                .color(count.paint()),
        );
    let step = |key: &'static str, name: &'static str, icon, by: isize| {
        round(
            key,
            name,
            glyph(icon, p.icon.size.lg, r.text.text.paint()),
            p,
        )
        .preserve_focus_on_press(true)
        .on_press(move |d: &mut Desk| d.step_find(by))
    };
    let done = touchable(
        row()
            .key("phone-find-done")
            .role("button")
            .label("Close find")
            .min_w(s.touch_min)
            .min_h(s.touch_min)
            .px(s.s2)
            .radius(p.radius.full)
            .center(),
        p,
    )
    .on_press_tx(|d: &mut Desk, tx| {
        d.close_find();
        tx.blur();
    })
    .child(
        text("Done")
            .font(p.ty.body.semibold())
            .color(r.accent.accent_text.paint())
            .a11y_hidden(),
    );
    Some(
        row()
            .key("phone-find")
            .w_full()
            .px(s.s3)
            .py(s.s2)
            .gap(s.s2)
            .items_center()
            .label("Find in terminal")
            .transition(super::motion::sheet(p))
            .child(capsule)
            .child(step(
                "phone-find-older",
                "Older match",
                &icons::ARROW_UP,
                -1,
            ))
            .child(step(
                "phone-find-newer",
                "Newer match",
                &icons::ARROW_DOWN,
                1,
            ))
            .child(done),
    )
}
