//! The phone form: a live terminal with glass chrome outside its viewport:
//! a top bar, navigation toolbar, tabs, accessory bar and keyboard. What
//! Rune can't do on a phone yet (safe areas, the system keyboard) is drawn
//! here instead; clients/rune/MOBILE.md lists each piece and what replaces it.

use rune::{Color, Cursor, Cx, El, Key, TextStyle, col, el, row, text, tok};

use super::{
    Ink, TITLEBAR_H, icons, inbox, machines, news, overlay, palette, panes, settings, spatial,
    status, tabs,
};
use crate::desk::Desk;
use crate::desk::phone::{Latch, Phone, Soft};
use crate::keys::Shortcut;

mod keyboard;
use keyboard::keyboard;

const TITLE: TextStyle = TextStyle::new(16.0, 600.0);
const SUBTITLE: TextStyle = TextStyle::new(13.0, 400.0);
const CAP_WORD: TextStyle = TextStyle::new(15.0, 500.0);
const CHIP: TextStyle = TextStyle::new(16.0, 500.0);
const TAB: TextStyle = TextStyle::new(14.0, 500.0);
/// A round control: the 44-point touch target.
const ROUND: f32 = 44.0;
/// The top bar's capsule and the gap under it: where a banner starts.
pub(super) const TOP_BAR: f32 = ROUND + 4.0 + 8.0;
/// Room for the status bar, which Rune doesn't report on a phone yet.
const STATUS_INSET: f32 = 12.0;

pub fn view(desk: &Desk, cx: &mut Cx<Desk>) -> El {
    let p = ink(desk, cx.accent());
    let fallback = Phone::default();
    let phone = desk.phone.as_ref().unwrap_or(&fallback);
    let keyboard_visible = desk.phone_keyboard_visible();
    // Glass floats beside the terminal, which may not have taken a newly
    // chosen theme yet: its ink contrasts with the ground actually drawn.
    let drawn = ground_ink(terminal_ground(desk), cx.accent(), desk.settings.theme);
    let controls = if desk.settings_open { &p } else { &drawn };
    cx.foreground(p.text);
    let stage = phone_stage(desk, &p, cx);
    let screen = col()
        .key("phone-screen")
        .w_full()
        .h_full()
        .pt(top_inset(desk, cx))
        .children((!desk.settings_open).then(|| top_bar(desk, controls)))
        .child(stage)
        .children(
            (!desk.settings_open)
                .then(|| tab_pills(desk, controls))
                .flatten(),
        );
    let bar = (!desk.settings_open || keyboard_visible).then(|| accessory(desk, phone, controls));
    let fill = edge_fill(desk, &p);
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
                .then(|| keyboard(phone, keyboard_visible, controls)),
        )
}

/// The terminal's own ground runs under everything, as on a phone. Before
/// a terminal is on screen it is the theme's, or the black a terminal
/// starts on: never the system's light page behind a dark launch.
fn terminal_ground(desk: &Desk) -> Color {
    let frame = desk
        .client
        .as_ref()
        .zip(desk.shown.as_ref())
        .and_then(|(client, shown)| client.acquire_view(shown.view?));
    frame.map_or_else(
        || {
            let theme = desk.settings.theme;
            theme.map_or(Color::BLACK, |theme| crate::theme::color(theme.background))
        },
        |frame| crate::grid::background(&frame),
    )
}

/// A phone shows one surface at a time, so its sheets and pages take the
/// chosen theme's ink, or with none the terminal ground's, never the
/// system appearance: a light-mode phone would otherwise open white sheets
/// over a dark terminal.
fn ink(desk: &Desk, accent: Color) -> Ink {
    match desk.settings.theme {
        Some(theme) => super::ink(theme.dark, accent, Some(theme)),
        None => ground_ink(terminal_ground(desk), accent, None),
    }
}

fn edge_fill(desk: &Desk, p: &Ink) -> Color {
    if desk.settings_open {
        p.sidebar
    } else {
        terminal_ground(desk)
    }
}

/// The phone's edge colour, for a native host to continue under the status
/// bar and home indicator it keeps outside the surface.
pub(crate) fn edge_color(desk: &Desk) -> Color {
    let accent = rune::SystemAccent::DEFAULT.accent(true);
    edge_fill(desk, &ink(desk, accent))
}

fn overlays(root: El, desk: &Desk, p: &Ink) -> El {
    root.children(status::banner(desk, p))
        .children(desk.inbox.open.then(|| inbox::view(desk, p)))
        .children(machines::sheet(desk, p))
        .children(desk.news.open.then(|| news::view(desk, p)))
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

/// Tinted glass under a rim lit from above and a soft lift. Every surface
/// sits beside the terminal on its flat ground, never over its text, so
/// there is nothing behind one for a backdrop blur to show.
fn glass(surface: El, p: &Ink) -> El {
    surface
        .continuous_corners()
        .gradient(p.text.alpha(0.13), p.text.alpha(0.06))
        .border_gradient(1.0, p.text.alpha(0.3), p.text.alpha(0.05))
        .shadow(6.0, 22.0, p.shade)
}

/// A glass circle around `content`.
fn round(key: &'static str, name: &'static str, content: El, p: &Ink) -> El {
    let circle = el()
        .key(key)
        .size(ROUND, ROUND)
        .radius(ROUND / 2.0)
        .center();
    glass(circle, p)
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
    // With no terminal on screen the window title is only the app's name:
    // the capsule names the computer instead.
    let title = if desk.shown.is_some() {
        full.strip_suffix(&format!(" — {session}"))
            .unwrap_or(full)
            .to_owned()
    } else {
        desk.machines.label().to_owned()
    };
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
    let place = glass(capsule, p)
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
            p,
        )
        .on_press_tx(|d: &mut Desk, tx| d.shortcut(Shortcut::Palette, tx))
    });
    let bell = round(
        "phone-news",
        news::bell_name(desk),
        glyph(&icons::BELL, 18.0, p.text),
        p,
    )
    .expanded(desk.news.open)
    .on_press(Desk::toggle_news)
    .children(news::unread_dot(desk, p, (27.0, 10.0), 7.0));
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
                p,
            )
            .on_press_tx(|d: &mut Desk, tx| d.shortcut(Shortcut::SwitchSession, tx)),
        )
        .child(place)
        .child(bell)
        .child(trailing)
}

/// The session's tabs by name, and a new one. A lone tab needs no strip:
/// its rows go to the terminal, and New Tab is in the commands.
fn tab_pills(desk: &Desk, p: &Ink) -> Option<El> {
    if desk.tabs.list.len() < 2 {
        return None;
    }
    let mut pills = glass(
        row()
            .key("phone-tabs")
            .shrink()
            .min_w(0.0)
            .padding(3.0)
            .gap(2.0)
            .radius(ROUND / 2.0)
            .items_center()
            .scroll_x(),
        p,
    );
    for (index, tab) in desk.tabs.list.iter().enumerate() {
        let waiting = tab
            .resources
            .iter()
            .any(|id| desk.activities.mark(id).needs_you());
        let ink = if tab.active { p.text } else { p.dim };
        pills = pills.child(
            row()
                .key(format!("phone-tab-{index}"))
                .h(ROUND - 6.0)
                .max_w(132.0)
                .px(12.0)
                .gap(6.0)
                .radius((ROUND - 6.0) / 2.0)
                .items_center()
                .fill(if tab.active {
                    p.current
                } else {
                    Color::TRANSPARENT
                })
                .motion(tok::motion::SNAPPY)
                .cursor(Cursor::Pointer)
                .label(format!("Tab {}", index + 1))
                .on_press(move |d: &mut Desk| d.select_tab(index))
                .children(waiting.then(|| el().size(6.0, 6.0).radius(3.0).fill(p.marks.alert)))
                .child(text(desk.tab_label(tab)).font(TAB).color(ink).truncate()),
        );
    }
    let new = round(
        "phone-tab-new",
        "New tab",
        glyph(&icons::PLUS, 16.0, p.text),
        p,
    )
    .on_press(Desk::new_tab);
    let strip = row()
        .w_full()
        .px(12.0)
        .py(6.0)
        .gap(8.0)
        .center()
        .child(pills)
        .child(new);
    // A modal owns the current group until it closes.
    Some(if desk.modal().is_some() {
        strip.inert()
    } else {
        strip
    })
}

/// A key on the accessory bar's strip. The strip scrolls instead of
/// squeezing its keys; at this width the sixth peeks in to say so.
fn chip(key: String, name: &'static str, label: &'static str, lit: bool, p: &Ink) -> El {
    el().key(key)
        .min_w(40.0)
        .h(36.0)
        .px(6.0)
        .radius(18.0)
        .center()
        .fill(if lit { p.accent } else { Color::TRANSPARENT })
        .motion(tok::motion::SNAPPY)
        .press(p.hover)
        .cursor(Cursor::Pointer)
        .label(name)
        .child(
            text(label)
                .font(CHIP)
                .color(if lit { p.on_accent } else { p.text }),
        )
}

/// What a terminal needs that a phone keyboard buries or lacks, in the
/// order it is reached for; the strip scrolls sideways through them.
const TYPED: [(&str, &str, char); 4] = [
    ("|", "Pipe", '|'),
    ("~", "Tilde", '~'),
    ("/", "Slash", '/'),
    ("-", "Hyphen", '-'),
];

fn strip(phone: &Phone, native_field: bool, p: &Ink) -> El {
    let send = |label: &'static str, name: &'static str, key: Key| {
        chip(format!("phone-key-{name}"), name, label, false, p)
            .on_press_tx(move |d: &mut Desk, tx| d.soft(Soft::Key(key.clone()), tx))
    };
    let latch = |latch: Latch, label: &'static str, name: &'static str| {
        chip(
            format!("phone-latch-{name}"),
            name,
            label,
            phone.latched(latch),
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
            .items_center()
            .scroll_x()
            // Keys past the edge fade out instead of being cut in half.
            .scroll_edge_fade(20.0),
        p,
    )
    .child(send("esc", "Escape", Key::Escape))
    .child(send("⇥", "Tab", Key::Tab));
    // A field being edited by the system keyboard takes neither chords nor
    // terminal keys.
    if native_field {
        return strip;
    }
    let interrupt = chip("phone-key-Interrupt".into(), "Control C", "⌃C", false, p)
        .on_press_tx(|d: &mut Desk, tx| d.soft_chord('c', tx));
    let paste = chip("phone-key-Paste".into(), "Paste", "Paste", false, p)
        .on_press_tx(|d: &mut Desk, tx| d.shortcut(Shortcut::Paste, tx));
    strip
        .child(latch(Latch::Ctrl, "⌃", "Control"))
        .child(latch(Latch::Alt, "⌥", "Option"))
        .child(latch(Latch::Shift, "⇧", "Shift"))
        .child(interrupt)
        .child(send("←", "Left", Key::Left))
        .child(send("↓", "Down", Key::Down))
        .child(send("↑", "Up", Key::Up))
        .child(send("→", "Right", Key::Right))
        .children(TYPED.map(|(label, name, typed)| {
            chip(format!("phone-key-{name}"), name, label, false, p)
                .on_press_tx(move |d: &mut Desk, tx| d.soft(Soft::Char(typed), tx))
        }))
        .child(paste)
}

fn accessory(desk: &Desk, phone: &Phone, p: &Ink) -> El {
    let native_field = desk.native.is_some() && crate::desk::native_input::editing(desk);
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
        .child(strip(phone, native_field, p))
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

    #[test]
    fn a_phone_takes_its_sheets_from_the_theme_or_the_ground_never_the_system() {
        let mut desk = Desk::settings_test_fixture().with_phone(true);
        desk.settings.theme = None;
        desk.settings_open = false;
        // No terminal yet: the black one starts on, so a light-mode phone
        // does not open on a white page between a dark launch and terminal.
        assert_eq!(terminal_ground(&desk), Color::BLACK);
        assert_eq!(edge_color(&desk), Color::BLACK);
        let sheets = ink(&desk, Color::WHITE);
        assert_eq!(crate::theme::on_fill(sheets.raised), Color::WHITE);
        let light = crate::theme::all().find(|theme| !theme.dark);
        desk.settings.theme = light;
        let sheets = ink(&desk, Color::WHITE);
        assert_eq!(crate::theme::on_fill(sheets.raised), Color::BLACK);
    }
}
