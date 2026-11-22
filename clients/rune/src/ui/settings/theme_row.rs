//! The phone's theme name and disclosure belong together, above its help text.
use rune::{El, col, el, row, text};

use super::{Desk, Paint, Tokens, primary, secondary};
use crate::ui::tokens::Voice;
use crate::ui::tokens::layout::SETTINGS_LINK as GEOMETRY;
use crate::ui::{icons, pressable};

pub(super) fn view(desk: &Desk, value: &str, detail: &str, t: &Tokens) -> El {
    let s = &t.space;
    let title = primary(desk, t);
    let heading = row()
        .grow(1.0)
        .min_w(0.0)
        .gap(s.s2)
        .flex_wrap()
        .justify_between()
        .items_center()
        // A scaled preferred width participates in wrapping; zero-basis grow
        // would squeeze the enlarged title beside the current theme.
        .child(
            text("Theme")
                .font(title.medium())
                .color(t.roles.text.text.paint())
                .w(GEOMETRY.phone_title_min * t.type_scale)
                .min_w(0.0)
                .shrink()
                .wrap(),
        )
        .child(
            text(value)
                .font(secondary(desk, t))
                .color(t.roles.text.dim.paint())
                .max_w(GEOMETRY.phone_value_max * t.type_scale)
                .min_w(0.0)
                .shrink()
                .wrap_anywhere(),
        );
    pressable("settings-theme", t)
        .w_full()
        .min_h(s.touch_min)
        .px(s.s4)
        .py(s.s3)
        .focusable(true)
        .label("Theme")
        .description(detail)
        .a11y_value(value)
        .on_press(Desk::settings_themes)
        .child(
            col()
                .w_full()
                .gap(s.s2)
                .child(
                    row().w_full().gap(s.s3).child(heading).child(
                        el().pt((title.line_px() - t.icon.size.md).max(0.0) / 2.0)
                            .child(
                                rune::icon(&icons::CHEVRON_RIGHT, t.icon.size.md)
                                    .color(t.roles.text.dim.paint()),
                            ),
                    ),
                )
                .child(
                    text(detail)
                        .font(secondary(desk, t))
                        .color(t.roles.text.dim.paint())
                        .wrap(),
                )
                .a11y_hidden(),
        )
}
