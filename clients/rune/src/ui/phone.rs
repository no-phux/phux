//! The phone form: a live terminal with glass chrome outside its viewport:
//! a top bar, navigation toolbar, tabs, accessory bar and keyboard. What
//! Rune can't do on a phone yet (safe areas, the system keyboard) is drawn
//! here instead; clients/rune/MOBILE.md lists each piece and what replaces it.

use rune::{Color, Cursor, Cx, Edge, El, Key, TextStyle, Transition, col, el, row, text, tok};

use super::{
    Ink, TITLEBAR_H, icons, inbox, machines, overlay, palette, panes, settings, spatial, status,
    tabs,
};
use crate::desk::Desk;
use crate::desk::phone::{Latch, Phone, Soft};
use crate::keys::Shortcut;

const TITLE: TextStyle = TextStyle::new(16.0, 600.0);
const SUBTITLE: TextStyle = TextStyle::new(13.0, 400.0);
const CAP: TextStyle = TextStyle::new(21.0, 400.0);
const CAP_WORD: TextStyle = TextStyle::new(15.0, 500.0);
const CHIP: TextStyle = TextStyle::new(16.0, 500.0);
/// A round control: the 44-point touch target.
const ROUND: f32 = 44.0;
const KEY_H: f32 = 42.0;
/// Room for the status bar and the home indicator, which Rune doesn't
/// report on a phone yet.
const STATUS_INSET: f32 = 12.0;
const HOME_INSET: f32 = 20.0;
/// How far glass blurs what it floats over. Rune blurs four elements a frame
/// (`MAX_BACKDROP_BLURS`): the top bar's three and the tab pills.
const GLASS_BLUR: f32 = 22.0;

const LETTERS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];
const SYMBOLS: [&str; 3] = ["1234567890", "-/:;()$&@\"", ".,?!'`|~"];

pub fn view(desk: &Desk, cx: &mut Cx<Desk>) -> El {
    let p = super::ink(cx.system_dark(), cx.accent(), desk.settings.theme);
    let fallback = Phone::default();
    let phone = desk.phone.as_ref().unwrap_or(&fallback);
    let keyboard_visible = desk.phone_keyboard_visible();
    // The terminal's own ground runs under everything, as on a phone.
    let ground = desk
        .client
        .as_ref()
        .zip(desk.shown.as_ref())
        .and_then(|(client, shown)| client.acquire_view(shown.view?))
        .map_or(p.bg, |frame| crate::grid::background(&frame));
    // Glass floats over the terminal, which may stay dark while UIKit/the
    // desktop prefers light chrome. Its ink must contrast with that ground,
    // not disappear into it. Fields/sheets keep their own opaque theme ink.
    let controls = if desk.settings_open {
        super::ink(cx.system_dark(), cx.accent(), desk.settings.theme)
    } else {
        ground_ink(ground, cx.accent(), desk.settings.theme)
    };
    cx.foreground(p.text);
    let stage = phone_stage(desk, &p, cx);
    let screen = col()
        .key("phone-screen")
        .w_full()
        .h_full()
        .pt(top_inset(desk, cx))
        .children((!desk.settings_open).then(|| top_bar(desk, &controls)))
        .child(stage)
        .children((!desk.settings_open).then(|| tab_pills(desk, &controls)));
    let bar = (!desk.settings_open || keyboard_visible).then(|| accessory(desk, phone, &controls));
    let fill = if desk.settings_open {
        p.sidebar
    } else {
        ground
    };
    // Both keyboards sit after every sheet, outside its measured viewport.
    // Rune treats post-modal controls as part of the modal: keys remain live
    // while the scrim still blocks the terminal and navigation behind it.
    let content = overlays(
        el().h(0.0).grow(1.0).w_full().min_h(0.0).child(screen),
        desk,
        &p,
    );
    col()
        .key("phone")
        .w_full()
        .h_full()
        .fill(fill)
        .child(content)
        .children(bar)
        .children(
            desk.native
                .is_none()
                .then(|| keyboard(phone, keyboard_visible, &controls)),
        )
}

fn overlays(root: El, desk: &Desk, p: &Ink) -> El {
    root.children(status::banner(desk, p))
        .children(desk.inbox.open.then(|| inbox::view(desk, p)))
        .children(machines::sheet(desk, p))
        .children(tabs::rename_sheet(desk, p))
        .children(
            desk.pointer
                .menu
                .map(|at| palette::context_menu(desk, at, p)),
        )
}

fn top_inset(desk: &Desk, cx: &Cx<Desk>) -> f32 {
    // Native UIKit already constrains the surface to its safe area. On a
    // desktop, traffic lights occupy the simulated status bar's place.
    if desk.native.is_some() {
        0.0
    } else if cx.titlebar_inset() > 0.0 {
        TITLEBAR_H
    } else {
        STATUS_INSET
    }
}

pub(super) fn ground_ink(ground: Color, accent: Color, theme: Option<&crate::theme::Theme>) -> Ink {
    let dark = crate::theme::on_fill(ground) == Color::WHITE;
    super::ink(dark, accent, theme.filter(|theme| theme.dark == dark))
}

/// Modals precede the drawn keyboard so its keys belong to their fields.
fn phone_stage(desk: &Desk, p: &Ink, cx: &mut Cx<Desk>) -> El {
    col()
        .key("phone-stage")
        .grow(1.0)
        .w_full()
        .min_h(0.0)
        .min_w(0.0)
        .px(if desk.settings_open { 0.0 } else { 4.0 })
        .child(if desk.settings_open {
            settings::view(desk, p)
        } else {
            panes::view(desk, p, cx)
        })
        .children(status::confirm_bar(desk, p))
        .children(overlay::toast(desk, p))
        .children(desk.palette_open.then(|| palette::view(desk, p, 8.0)))
}

/// Liquid glass: what it floats over, blurred (`blur`) and tinted, under a
/// rim lit from above and a soft lift.
fn glass(surface: El, blur: bool, p: &Ink) -> El {
    let surface = if blur {
        surface.backdrop_blur(GLASS_BLUR)
    } else {
        surface
    };
    surface
        .continuous_corners()
        .gradient(p.text.alpha(0.13), p.text.alpha(0.06))
        .border_gradient(1.0, p.text.alpha(0.3), p.text.alpha(0.05))
        .shadow(6.0, 22.0, p.shade)
}

/// A glass circle around `content`.
fn round(key: &'static str, name: &'static str, content: El, blur: bool, p: &Ink) -> El {
    let circle = el()
        .key(key)
        .size(ROUND, ROUND)
        .radius(ROUND / 2.0)
        .center();
    glass(circle, blur, p)
        .press(p.hover)
        .cursor(Cursor::Pointer)
        .label(name)
        .child(content)
}

fn glyph(icon: &'static rune::Icon, size: f32, color: Color) -> El {
    rune::icon(icon, size).color(color)
}

fn top_bar(desk: &Desk, p: &Ink) -> El {
    let session = desk
        .selected_session_name()
        .unwrap_or_else(|| "phux".to_owned());
    let full = desk.window_title();
    let title = full
        .strip_suffix(&format!(" — {session}"))
        .unwrap_or(full)
        .to_owned();
    let tile = el()
        .size(30.0, 30.0)
        .radius(8.0)
        .continuous_corners()
        .center()
        .fill(p.text.alpha(0.1))
        .child(glyph(&icons::PROMPT, 15.0, p.marks.good));
    let capsule = row()
        .key("phone-place")
        .grow(1.0)
        .min_w(0.0)
        .h(ROUND + 4.0)
        .pl(8.0)
        .pr(16.0)
        .gap(10.0)
        .radius((ROUND + 4.0) / 2.0)
        .items_center();
    let place = glass(capsule, true, p)
        .press(p.hover)
        .cursor(Cursor::Pointer)
        .label("Change session")
        .on_press_tx(|d: &mut Desk, tx| d.shortcut(Shortcut::SwitchSession, tx))
        .child(tile)
        .child(
            col()
                .min_w(0.0)
                .shrink()
                .gap(1.0)
                .child(text(title).font(TITLE).color(p.text).truncate())
                .child(
                    row()
                        .gap(4.0)
                        .items_center()
                        .min_w(0.0)
                        .child(text(session).font(SUBTITLE).color(p.dim).truncate())
                        .child(glyph(&icons::CHEVRON, 12.0, p.dim)),
                ),
        );
    let trailing = spatial::minimap(desk, p).unwrap_or_else(|| {
        round(
            "phone-more",
            "Commands",
            glyph(&icons::MORE, 20.0, p.text),
            true,
            p,
        )
        .on_press_tx(|d: &mut Desk, tx| d.shortcut(Shortcut::Palette, tx))
    });
    row()
        .key("phone-top")
        .w_full()
        .px(12.0)
        .pb(8.0)
        .gap(10.0)
        .items_center()
        .window_drag()
        .child(
            round(
                "phone-back",
                "Sessions",
                glyph(&icons::CHEVRON_LEFT, 18.0, p.text),
                true,
                p,
            )
            .on_press_tx(|d: &mut Desk, tx| d.shortcut(Shortcut::SwitchSession, tx)),
        )
        .child(place)
        .child(trailing)
}

/// The session's tabs as a row of prompts, and a new one.
fn tab_pills(desk: &Desk, p: &Ink) -> El {
    let count = desk.tabs.list.len().max(1);
    let mut pills = glass(
        row()
            .key("phone-tabs")
            .padding(3.0)
            .gap(2.0)
            .radius(17.0)
            .items_center(),
        true,
        p,
    );
    for index in 0..count {
        let active = desk.tabs.list.get(index).is_none_or(|tab| tab.active);
        pills = pills.child(
            el().key(format!("phone-tab-{index}"))
                .w(40.0)
                .h(28.0)
                .radius(14.0)
                .center()
                .fill(if active {
                    p.current
                } else {
                    Color::TRANSPARENT
                })
                .motion(tok::motion::SNAPPY)
                .cursor(Cursor::Pointer)
                .label(format!("Tab {}", index + 1))
                .on_press(move |d: &mut Desk| d.select_tab(index))
                .child(glyph(
                    &icons::PROMPT,
                    15.0,
                    if active { p.text } else { p.dim },
                )),
        );
    }
    let new = glass(
        el().key("phone-tab-new")
            .size(34.0, 34.0)
            .radius(17.0)
            .center(),
        false,
        p,
    )
    .press(p.hover)
    .cursor(Cursor::Pointer)
    .label("New tab")
    .on_press(Desk::new_tab)
    .child(glyph(&icons::PLUS, 15.0, p.text));
    let strip = row()
        .w_full()
        .py(8.0)
        .gap(8.0)
        .center()
        .child(pills)
        .child(new);
    // A modal owns the current group until it closes.
    if desk.modal().is_some() {
        strip.inert()
    } else {
        strip
    }
}

/// A key on the accessory bar's strip.
fn chip(key: String, name: &'static str, content: El, lit: bool, p: &Ink) -> El {
    el().key(key)
        .grow(1.0)
        .h(36.0)
        .radius(18.0)
        .center()
        .fill(if lit { p.accent } else { Color::TRANSPARENT })
        .motion(tok::motion::SNAPPY)
        .press(p.hover)
        .cursor(Cursor::Pointer)
        .label(name)
        .child(content)
}

fn accessory(desk: &Desk, phone: &Phone, p: &Ink) -> El {
    let native_field = desk.native.is_some() && crate::desk::native_input::editing(desk);
    let send = |label: &'static str, name: &'static str, key: Key| {
        chip(
            format!("phone-key-{name}"),
            name,
            text(label).font(CHIP).color(p.text),
            false,
            p,
        )
        .on_press_tx(move |d: &mut Desk, tx| d.soft(Soft::Key(key.clone()), tx))
    };
    let latch = |latch: Latch, label: &'static str, name: &'static str| {
        let lit = phone.latched(latch);
        chip(
            format!("phone-latch-{name}"),
            name,
            text(label)
                .font(CHIP)
                .color(if lit { p.on_accent } else { p.text }),
            lit,
            p,
        )
        .on_press(move |d: &mut Desk| d.toggle_latch(latch))
    };
    let strip = glass(
        row()
            .key("phone-strip")
            .grow(1.0)
            .min_w(0.0)
            .h(ROUND)
            .px(4.0)
            .gap(2.0)
            .radius(ROUND / 2.0)
            .items_center(),
        false,
        p,
    )
    .child(send("esc", "Escape", Key::Escape))
    .child(send("⇥", "Tab", Key::Tab))
    .children((!native_field).then(|| latch(Latch::Ctrl, "⌃", "Control")))
    .children((!native_field).then(|| latch(Latch::Alt, "⌥", "Option")))
    .children((!native_field).then(|| latch(Latch::Shift, "⇧", "Shift")));
    let pad = el()
        .key("phone-pad")
        .size(ROUND, ROUND)
        .radius(ROUND / 2.0)
        .center()
        .press(p.hover)
        .cursor(Cursor::Grab)
        .label("Arrow keys: drag")
        .on_drag(|d: &mut Desk, drag, tx| d.pad_drag(drag.phase, drag.dx, drag.dy, tx))
        .child(glyph(&icons::PAD, 20.0, p.text));
    let pair = glass(
        row().key("phone-pair").radius(ROUND / 2.0).items_center(),
        false,
        p,
    )
    .child(pad)
    .child(keyboard_control(desk, native_field, p));
    let command = round(
        "phone-command",
        if desk.settings_open {
            "Done editing Settings"
        } else {
            "Commands"
        },
        text(if desk.settings_open { "✓" } else { "⌘" })
            .font(CHIP)
            .color(p.text),
        false,
        p,
    )
    .on_press_tx(Desk::phone_commands);
    row()
        .key("phone-accessory")
        .preserve_focus_on_press(true)
        .w_full()
        .px(10.0)
        .pb(8.0)
        .gap(8.0)
        .items_center()
        .child(command)
        .child(strip)
        .child(pair)
}

fn keyboard_control(desk: &Desk, native_field: bool, p: &Ink) -> El {
    if native_field {
        return el()
            .key("phone-native-done")
            .focusable(true)
            .size(ROUND, ROUND)
            .center()
            .label("Done editing")
            .cursor(Cursor::Pointer)
            .on_press_tx(crate::desk::native_input::done)
            .child(text("Done").font(CAP_WORD).color(p.text));
    }
    el().key("phone-keyboard-toggle")
        .size(ROUND, ROUND)
        .radius(ROUND / 2.0)
        .center()
        .fill(if desk.phone_keyboard_visible() {
            p.current
        } else {
            Color::TRANSPARENT
        })
        .press(p.hover)
        .cursor(Cursor::Pointer)
        .label(if desk.phone_keyboard_visible() {
            "Hide keyboard"
        } else {
            "Show keyboard"
        })
        .on_press_tx(Desk::toggle_keyboard)
        .child(glyph(&icons::KEYBOARD, 20.0, p.text))
}

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
fn keyboard(phone: &Phone, visible: bool, p: &Ink) -> El {
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
    glass(board, false, p)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glass_ink_tracks_its_terminal_ground_not_the_os_appearance() {
        for (ground, expected) in [(Color::BLACK, Color::WHITE), (Color::WHITE, Color::BLACK)] {
            let ink = ground_ink(ground, Color::WHITE, None);
            assert_eq!(
                crate::theme::on_fill(ink.text),
                crate::theme::on_fill(expected)
            );
            assert!(crate::theme::contrast(ink.text, ground) >= 4.5);
        }
    }
}
