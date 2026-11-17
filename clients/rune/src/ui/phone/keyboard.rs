//! The drawn keyboard of the desktop-hosted phone form. A native host has
//! the system's and never draws this (clients/rune/MOBILE.md).
//!
//! Keys sit on the wash ramp over an opaque board: letters on the selected
//! wash, function keys on the current wash, a press adding the press wash.
//! Rows are a full touch target tall; their width is the system
//! keyboard's, ten letters to a row.

use crate::ui::tokens::Voice;
use rune::{Color, Edge, El, Key, Transition, col, el, row, text};

use super::{Look, glyph, surface, touchable};
use crate::desk::Desk;
use crate::desk::phone::{Phone, Soft};
use crate::ui::icons;
use crate::ui::tokens::Paint;

const LETTERS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];
const SYMBOLS: [&str; 3] = ["1234567890", "-/:;()$&@\"", ".,?!'`|~"];

/// A keyboard key; `fill` sets letters apart from the keys around them.
fn cap(key: String, name: String, content: El, fill: Color, p: &Look) -> El {
    touchable(
        el().key(key)
            .h(p.space.touch_min)
            .radius(p.radius.sm)
            .continuous_corners()
            .center()
            .fill(fill),
        p,
    )
    .label(name)
    .child(content)
}

/// A wide key (the page and return keys): two of the grid's steps.
fn wide(p: &Look) -> f32 {
    p.space.s16 + p.space.s6
}

/// The drawn keyboard, until Rune brings up the system's on a phone.
pub(super) fn keyboard(phone: &Phone, visible: bool, p: &Look) -> El {
    let s = &p.space;
    if !visible {
        // Room for the home indicator, which Rune doesn't report yet.
        return el().key("phone-home").h(s.s5);
    }
    let ink = p.roles.text.text.paint();
    let letter = p.roles.wash.selected.paint();
    let function = p.roles.wash.current.paint();
    let rows = if phone.symbols { SYMBOLS } else { LETTERS };
    let capital = phone.capital && !phone.symbols;
    let char_key = |c: char| {
        let shown = if capital { c.to_ascii_uppercase() } else { c };
        cap(
            format!("phone-cap-{}", u32::from(c)),
            shown.to_string(),
            text(shown.to_string()).font(p.ty.title).color(ink),
            letter,
            p,
        )
        .grow(1.0)
        .min_w(0.0)
        .on_press_tx(move |d: &mut Desk, tx| d.soft(Soft::Char(c), tx))
    };
    let line = |chars: &str, inset: f32| {
        let mut line = row().w_full().gap(s.s1).px(inset);
        for c in chars.chars() {
            line = line.child(char_key(c));
        }
        line
    };
    let backspace = cap(
        "phone-cap-backspace".to_owned(),
        "Delete".to_owned(),
        glyph(&icons::BACKSPACE, p.icon.size.lg, ink),
        function,
        p,
    )
    .w(s.touch_min)
    .on_press_tx(|d: &mut Desk, tx| d.soft(Soft::Key(Key::Backspace), tx));
    let third = row()
        .w_full()
        .gap(s.s1)
        .child(shift_cap(phone, capital, p))
        .child(line(rows[2], s.s2).grow(1.0).min_w(0.0))
        .child(backspace);
    let word = |label: &'static str, key: &'static str| {
        cap(
            key.to_owned(),
            label.to_owned(),
            text(label).font(p.ty.label.medium()).color(ink),
            function,
            p,
        )
    };
    let bottom = row()
        .w_full()
        .gap(s.s1)
        .child(
            word(if phone.symbols { "ABC" } else { "123" }, "phone-cap-page")
                .w(wide(p))
                .on_press(Desk::toggle_symbols),
        )
        .child(
            cap(
                "phone-cap-space".to_owned(),
                "Space".to_owned(),
                el(),
                letter,
                p,
            )
            .grow(1.0)
            .on_press_tx(|d: &mut Desk, tx| d.soft(Soft::Char(' '), tx)),
        )
        .child(
            cap(
                "phone-cap-return".to_owned(),
                "Return".to_owned(),
                glyph(&icons::RETURN, p.icon.size.lg, ink),
                function,
                p,
            )
            .w(wide(p))
            .on_press_tx(|d: &mut Desk, tx| d.soft(Soft::Key(Key::Enter), tx)),
        );
    let board = col()
        .key("phone-keyboard")
        .preserve_focus_on_press(true)
        .w_full()
        .px(s.s1)
        .pt(s.s2)
        .pb(s.s5)
        .gap(s.s2)
        .radius(p.radius.xl);
    // The system keyboard it stands in for slides in and out.
    surface(board, p)
        .transition(Transition::move_edge(Edge::Bottom).springs(p.motion.drawer))
        .child(line(rows[0], 0.0))
        .child(line(rows[1], if phone.symbols { 0.0 } else { s.s4 }))
        .child(third)
        .child(bottom)
}

fn shift_cap(phone: &Phone, capital: bool, p: &Look) -> El {
    if phone.symbols {
        return el().w(p.space.touch_min);
    }
    let (fill, ink) = if capital {
        (
            p.roles.accent.accent.paint(),
            p.roles.accent.on_accent.paint(),
        )
    } else {
        (p.roles.wash.current.paint(), p.roles.text.text.paint())
    };
    cap(
        "phone-cap-shift".to_owned(),
        "Shift".to_owned(),
        glyph(&icons::SHIFT, p.icon.size.lg, ink),
        fill,
        p,
    )
    .w(p.space.touch_min)
    .on_press(Desk::toggle_capital)
}
