//! The accessory bar: Commands, a scrolling strip of the keys a terminal
//! needs that a phone keyboard buries, and the arrow pad beside the
//! keyboard toggle. Every key is a full touch target.

use rune::{Color, Cursor, El, Key, el, row, text};

use super::{Look, glyph, icons, medium, round, surface, touchable};
use crate::desk::Desk;
use crate::desk::phone::{Latch, Phone, Soft};
use crate::keys::Shortcut;
use crate::ui::tokens::Paint;

/// A key on the accessory bar's strip, a full touch target. The strip
/// scrolls instead of squeezing its keys; at this width the sixth peeks in
/// to say so.
fn chip(key: String, name: &'static str, label: &'static str, lit: bool, p: &Look) -> El {
    let (fill, ink) = if lit {
        (
            p.roles.accent.accent.paint(),
            p.roles.accent.on_accent.paint(),
        )
    } else {
        (Color::TRANSPARENT, p.roles.text.text.paint())
    };
    let chip = el()
        .key(key)
        .min_w(p.round())
        .h(p.round())
        .px(p.space.s2)
        .radius(p.radius.full)
        .center()
        .fill(fill)
        .motion(p.motion.control.enter)
        .label(name)
        .child(text(label).font(medium(p.ty.lead)).color(ink));
    touchable(chip, p)
}

/// What a terminal needs that a phone keyboard buries or lacks, in the
/// order it is reached for; the strip scrolls sideways through them.
const TYPED: [(&str, &str, char); 4] = [
    ("|", "Pipe", '|'),
    ("~", "Tilde", '~'),
    ("/", "Slash", '/'),
    ("-", "Hyphen", '-'),
];

fn strip(phone: &Phone, native_field: bool, p: &Look) -> El {
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
    let strip = surface(
        row()
            .key("phone-strip")
            .grow(1.0)
            .min_w(0.0)
            .h(p.round())
            .radius(p.radius.full)
            .items_center()
            .scroll_x()
            // Keys past the edge fade out instead of being cut in half.
            .scroll_edge_fade(p.space.s5),
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

pub(super) fn accessory(desk: &Desk, phone: &Phone, p: &Look) -> El {
    let s = &p.space;
    let native_field = desk.native.is_some() && crate::desk::native_input::editing(desk);
    let ink = p.roles.text.text.paint();
    let pad = touchable(
        el().key("phone-pad")
            .size(p.round(), p.round())
            .radius(p.radius.full)
            .center(),
        p,
    )
    .cursor(Cursor::Grab)
    .label("Arrow keys: drag")
    .touch_drags_at_once()
    .on_drag(|d: &mut Desk, drag, tx| d.pad_drag(drag.phase, drag.dx, drag.dy, tx))
    .child(glyph(&icons::PAD, p.icon.size.lg, ink));
    let pair = surface(
        row().key("phone-pair").radius(p.radius.full).items_center(),
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
            .font(medium(p.ty.lead))
            .color(ink),
        p,
    )
    .on_press_tx(Desk::phone_commands);
    row()
        .key("phone-accessory")
        .preserve_focus_on_press(true)
        .w_full()
        .px(s.s2)
        .pb(s.s2)
        .gap(s.s2)
        .items_center()
        .child(command)
        .child(strip(phone, native_field, p))
        .child(pair)
}

fn keyboard_control(desk: &Desk, native_field: bool, p: &Look) -> El {
    let ink = p.roles.text.text.paint();
    if native_field {
        return touchable(
            el().key("phone-native-done")
                .focusable(true)
                .size(p.round(), p.round())
                .radius(p.radius.full)
                .center(),
            p,
        )
        .label("Done editing")
        .on_press_tx(crate::desk::native_input::done)
        .child(text("Done").font(medium(p.ty.label)).color(ink));
    }
    let shown = desk.phone_keyboard_visible();
    touchable(
        el().key("phone-keyboard-toggle")
            .size(p.round(), p.round())
            .radius(p.radius.full)
            .center()
            .fill(if shown {
                p.roles.wash.selected.paint()
            } else {
                Color::TRANSPARENT
            }),
        p,
    )
    .label(if shown {
        "Hide keyboard"
    } else {
        "Show keyboard"
    })
    .on_press_tx(Desk::toggle_keyboard)
    .child(glyph(&icons::KEYBOARD, p.icon.size.lg, ink))
}
