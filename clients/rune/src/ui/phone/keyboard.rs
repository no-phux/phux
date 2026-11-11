//! The drawn keyboard of the desktop-hosted phone form. A native host has
//! the system's and never draws this (clients/rune/MOBILE.md).

use rune::{Color, Cursor, Edge, El, Key, TextStyle, Transition, col, el, row, text};

use super::{CAP_WORD, glass, glyph};
use crate::desk::Desk;
use crate::desk::phone::{Phone, Soft};
use crate::ui::{Ink, icons};

const CAP: TextStyle = TextStyle::new(21.0, 400.0);
const KEY_H: f32 = 42.0;
/// Room for the home indicator, which Rune doesn't report on a phone yet.
const HOME_INSET: f32 = 20.0;

const LETTERS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];
const SYMBOLS: [&str; 3] = ["1234567890", "-/:;()$&@\"", ".,?!'`|~"];

/// A keyboard key; `fill` sets letters apart from the keys around them.
fn cap(key: String, name: String, content: El, fill: Color, p: &Ink) -> El {
    el().key(key)
        .h(KEY_H)
        .radius(7.0)
        .continuous_corners()
        .center()
        .fill(fill)
        .press(p.text.alpha(0.4))
        .cursor(Cursor::Pointer)
        .label(name)
        .child(content)
}

/// The drawn keyboard, until Rune brings up the system's on a phone.
pub(super) fn keyboard(phone: &Phone, visible: bool, p: &Ink) -> El {
    if !visible {
        return el().key("phone-home").h(HOME_INSET);
    }
    let letter = p.text.alpha(0.22);
    let function = p.text.alpha(0.1);
    let rows = if phone.symbols { SYMBOLS } else { LETTERS };
    let capital = phone.capital && !phone.symbols;
    let char_key = |c: char| {
        let shown = if capital { c.to_ascii_uppercase() } else { c };
        cap(
            format!("phone-cap-{}", u32::from(c)),
            shown.to_string(),
            text(shown.to_string()).font(CAP).color(p.text),
            letter,
            p,
        )
        .grow(1.0)
        .min_w(0.0)
        .on_press_tx(move |d: &mut Desk, tx| d.soft(Soft::Char(c), tx))
    };
    let line = |chars: &str, inset: f32| {
        let mut line = row().w_full().gap(6.0).px(inset);
        for c in chars.chars() {
            line = line.child(char_key(c));
        }
        line
    };
    let backspace = cap(
        "phone-cap-backspace".to_owned(),
        "Delete".to_owned(),
        glyph(&icons::BACKSPACE, 20.0, p.text),
        function,
        p,
    )
    .w(KEY_H)
    .on_press_tx(|d: &mut Desk, tx| d.soft(Soft::Key(Key::Backspace), tx));
    let third = row()
        .w_full()
        .gap(6.0)
        .child(shift_cap(phone, capital, p))
        .child(line(rows[2], 8.0).grow(1.0).min_w(0.0))
        .child(backspace);
    let word = |label: &'static str, key: &'static str| {
        cap(
            key.to_owned(),
            label.to_owned(),
            text(label).font(CAP_WORD).color(p.text),
            function,
            p,
        )
    };
    let bottom = row()
        .w_full()
        .gap(6.0)
        .child(
            word(if phone.symbols { "ABC" } else { "123" }, "phone-cap-page")
                .w(88.0)
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
                glyph(&icons::RETURN, 20.0, p.text),
                function,
                p,
            )
            .w(88.0)
            .on_press_tx(|d: &mut Desk, tx| d.soft(Soft::Key(Key::Enter), tx)),
        );
    let board = col()
        .key("phone-keyboard")
        .preserve_focus_on_press(true)
        .w_full()
        .px(4.0)
        .pt(10.0)
        .pb(HOME_INSET)
        .gap(11.0)
        .radius(26.0);
    glass(board, p)
        .transition(Transition::move_edge(Edge::Bottom))
        .child(line(rows[0], 0.0))
        .child(line(rows[1], if phone.symbols { 0.0 } else { 18.0 }))
        .child(third)
        .child(bottom)
}

fn shift_cap(phone: &Phone, capital: bool, p: &Ink) -> El {
    if phone.symbols {
        return el().w(KEY_H);
    }
    cap(
        "phone-cap-shift".to_owned(),
        "Shift".to_owned(),
        glyph(&icons::SHIFT, 18.0, if capital { p.bg } else { p.text }),
        if capital { p.text } else { p.text.alpha(0.1) },
        p,
    )
    .w(KEY_H)
    .on_press(Desk::toggle_capital)
}
