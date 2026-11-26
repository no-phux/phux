//! The Developer and Labs rows that are not plain toggles.

use phux_config_core::labs::Lab;
use rune::{El, col, row, text};

use super::super::super::pressable;
use super::super::controls;
use super::{Entry, chevron};
use crate::desk::Desk;
use crate::ui::tokens::{Paint, Tokens, Voice};

/// A lab's switch: on as the file or the lab's default has it. Saving
/// writes `[labs]`, which every part of phux reads.
pub(super) fn lab_toggle(lab: &'static Lab, desk: &Desk, t: &Tokens) -> El {
    controls::toggle(
        desk,
        controls::Toggle {
            key: format!("set-lab-{}", lab.id),
            title: lab.title,
            detail: lab.detail,
            on: desk.lab_on(lab),
            press: move |d: &mut Desk| d.toggle_lab(lab),
            enabled: true,
        },
        t,
    )
}

pub(super) fn diagnostics(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    pressable("copy-diagnostics", t)
        .w_full()
        .min_h(s.touch_min)
        .padding(s.s4)
        .focusable(true)
        .label("Copy diagnostics")
        .description(Entry::Diagnostics.detail())
        .on_press_tx(|d: &mut Desk, tx| d.copy_diagnostics(tx))
        .child(
            row()
                .w_full()
                .gap(s.s3)
                .items_center()
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(s.s0_5)
                        .child(
                            text(if desk.updater.copied {
                                "Copied"
                            } else {
                                "Copy diagnostics"
                            })
                            .font(super::super::primary(desk, t).medium())
                            .color(t.roles.text.text.paint()),
                        )
                        .child(
                            text(Entry::Diagnostics.detail())
                                .font(super::super::secondary(desk, t))
                                .color(t.roles.text.dim.paint())
                                .wrap(),
                        ),
                )
                .child(chevron(t))
                .a11y_hidden(),
        )
}
