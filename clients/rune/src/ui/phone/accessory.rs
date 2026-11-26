//! The accessory bar: Commands, a scrolling strip of the keys a terminal
//! needs that a phone keyboard buries, and a pinned keyboard toggle and
//! Return (or Done while the system keyboard edits a field).
//! The pad joins the strip when editing or making room for large text.
//! Every key is a full touch target.

use crate::ui::tokens::Voice;
use rune::{Color, Cursor, El, Key, Live, col, el, row, text};

use super::{Look, glyph, icons, surface, touchable};
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
        .min_h(p.round())
        .px(p.space.s2)
        .py(p.space.s1)
        .radius(p.radius.full)
        .center()
        .fill(fill)
        .motion(p.motion.control.enter)
        .label(name)
        .child(text(label).font(p.ty.lead.medium()).color(ink));
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

fn strip(
    phone: &Phone,
    native_field: bool,
    pinned: [Option<El>; 2],
    enter: Option<El>,
    p: &Look,
) -> El {
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
            .w_full()
            .min_w(0.0)
            .min_h(p.round())
            .radius(p.radius.full)
            .items_center()
            .scroll_x()
            .description("Swipe sideways for more keys")
            // Keys past the edge fade out instead of being cut in half.
            .scroll_edge_fade(p.space.s5),
        p,
    )
    .children(enter)
    .child(send("esc", "Escape", Key::Escape))
    .child(send("⇥", "Tab", Key::Tab));
    // A field being edited by the system keyboard takes neither chords nor
    // terminal keys.
    if native_field {
        return key_rail(strip.children(pinned.into_iter().flatten()), p);
    }
    let interrupt = chip("phone-key-Interrupt".into(), "Control C", "⌃C", false, p)
        .on_press_tx(|d: &mut Desk, tx| d.soft_chord('c', tx));
    let paste = chip("phone-key-Paste".into(), "Paste", "Paste", false, p)
        .on_press_tx(|d: &mut Desk, tx| d.shortcut(Shortcut::Paste, tx));
    let strip = strip
        .child(latch(Latch::Ctrl, "⌃", "Control"))
        .child(latch(Latch::Alt, "⌥", "Option"))
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
        .children(pinned.into_iter().flatten());
    key_rail(strip, p)
}

/// A persistent overflow cue in the bar's existing bottom inset. It is a
/// noninteractive indicator, not a tiny extra touch target over a key.
/// Live scroll geometry updates it without rebuilding the field or its bar.
fn key_rail(strip: El, p: &Look) -> El {
    let track = Live::new([0.0; 4]);
    let thumb = Live::new([0.0; 4]);
    let shown = Live::new(0.0);
    let (at_track, at_thumb, at_shown) = (track.clone(), thumb.clone(), shown.clone());
    let (inset, height, track_height, slot, minimum) = (
        p.space.s2,
        p.space.s1,
        p.space.hairline,
        p.space.s2,
        p.space.s6,
    );
    let strip = strip.on_scroll_offset(move |at| {
        // As Rune's own scroller does: follow this frame, not a spring's
        // destination width, while keeping the logical scroll range.
        let viewport = at.drawn[0];
        let width = (viewport - 2.0 * inset).max(0.0);
        let content = (viewport + at.max[0]).max(1.0);
        let length = (width * viewport / content).clamp(minimum.min(width), width);
        let fraction = (at.offset[0] / at.max[0].max(1.0)).clamp(0.0, 1.0);
        let y = (slot - height) / 2.0;
        at_track.set([inset, (slot - track_height) / 2.0, width, track_height]);
        at_thumb.set([inset + (width - length) * fraction, y, length, height]);
        at_shown.set(if at.max[0] > 0.5 { 1.0 } else { 0.0 });
    });
    let cue = el()
        .key("phone-strip-cue")
        .w_full()
        .h(slot)
        .inert()
        .a11y_hidden()
        .live_opacity(&shown)
        .child(
            el().key("phone-strip-track")
                .absolute(0.0, 0.0)
                .live_rect(&track)
                .radius(p.radius.full)
                .fill(p.roles.border.base.paint()),
        )
        .child(
            el().key("phone-strip-thumb")
                .absolute(0.0, 0.0)
                .live_rect(&thumb)
                .radius(p.radius.full)
                .fill(p.roles.text.dim.paint()),
        );
    col()
        .key("phone-strip-rail")
        .grow(1.0)
        .min_w(0.0)
        .child(strip)
        .child(cue)
}

pub(super) fn accessory(desk: &Desk, phone: &Phone, p: &Look) -> El {
    let s = &p.space;
    let native_field = desk.native.is_some() && crate::desk::native_input::editing(desk);
    let ink = p.roles.text.text.paint();
    let pad = touchable(
        el().key("phone-pad")
            .min_w(p.round())
            .min_h(p.round())
            .radius(p.radius.full)
            .center(),
        p,
    )
    .cursor(Cursor::Grab)
    .label("Arrow keys: drag")
    .touch_drags_at_once()
    .on_drag(|d: &mut Desk, drag, tx| d.pad_drag(drag.phase, drag.dx, drag.dy, tx))
    .child(glyph(&icons::PAD, p.icon.size.lg, ink));
    // Reserve the complete Done label, not a 44-point fragment of it.
    // Moving the pad into the strip leaves enough room to reveal every
    // field key, even on a small phone at the largest text size. Its
    // immediate drag capture still owns arrows rather than strip scrolling.
    let scrolling_pad =
        native_field || p.type_scale >= crate::ui::tokens::layout::PHONE.stacked_title_scale;
    // Only a native host has photos and files to attach, and only a
    // terminal takes them. It ends the strip beside Paste, so the pinned
    // pair keeps room for Return on the narrowest phone.
    let clip = (desk.native.is_some() && !native_field && !desk.settings_open).then(|| attach(p));
    let (strip_pad, pair_pad) = if scrolling_pad {
        ([clip, Some(pad)], None)
    } else {
        ([clip, None], Some(pad))
    };
    // Return stays pinned under the right thumb unless the largest text
    // leaves no room beside it: then it leads the strip, first in view.
    let enter = (!native_field).then(|| return_key(p));
    let (strip_enter, pair_enter) = if scrolling_pad {
        (enter, None)
    } else {
        (None, enter)
    };
    let pair = surface(
        row().key("phone-pair").radius(p.radius.full).items_center(),
        p,
    )
    .children(pair_pad)
    .child(keyboard_control(desk, native_field, p))
    .children(pair_enter);
    let command = touchable(
        surface(
            el().key("phone-command")
                .min_w(p.round())
                .min_h(p.round())
                .px(s.s2)
                .py(s.s1)
                .radius(p.radius.full)
                .center(),
            p,
        ),
        p,
    )
    .label(if desk.settings_open {
        "Done editing Settings"
    } else {
        "Commands"
    })
    .child(
        text(if desk.settings_open { "✓" } else { "⌘" })
            .font(p.ty.lead.medium())
            .color(ink),
    )
    .on_press_tx(Desk::phone_commands);
    row()
        .key("phone-accessory")
        .preserve_focus_on_press(true)
        .w_full()
        .px(s.s2)
        .gap(s.s2)
        .items_center()
        .child(el().pb(s.s2).child(command))
        .child(strip(phone, native_field, strip_pad, strip_enter, p))
        .child(el().pb(s.s2).child(pair))
}

/// Return, pinned under the right thumb where a keyboard's return sits,
/// so the one key a terminal reaches for most never scrolls away. A field
/// the system keyboard edits has that keyboard's own return instead.
fn return_key(p: &Look) -> El {
    let ink = p.roles.accent.on_accent.paint();
    touchable(
        el().key("phone-key-Enter")
            .min_w(p.round())
            .min_h(p.round())
            .radius(p.radius.full)
            .center()
            .fill(p.roles.accent.accent.paint()),
        p,
    )
    .label("Return")
    .on_press_tx(|d: &mut Desk, tx| d.soft(Soft::Key(Key::Enter), tx))
    .child(glyph(&icons::RETURN, p.icon.size.lg, ink))
}

/// The paperclip: the phone's photos, camera and files, for the agent in
/// this pane. What ⇧⌘O is to a desktop's screenshots.
fn attach(p: &Look) -> El {
    touchable(
        el().key("phone-attach")
            .min_w(p.round())
            .min_h(p.round())
            .radius(p.radius.full)
            .center(),
        p,
    )
    .label("Attach a photo or file")
    .on_press_tx(|d: &mut Desk, tx| d.shortcut(Shortcut::Screenshot, tx))
    .child(glyph(
        &icons::PAPERCLIP,
        p.icon.size.lg,
        p.roles.text.text.paint(),
    ))
}

fn keyboard_control(desk: &Desk, native_field: bool, p: &Look) -> El {
    let ink = p.roles.text.text.paint();
    if native_field {
        return touchable(
            el().key("phone-native-done")
                .focusable(true)
                .min_w(p.round())
                .min_h(p.round())
                .px(p.space.s2)
                .py(p.space.s1)
                .radius(p.radius.full)
                .center(),
            p,
        )
        .label("Done editing")
        .on_press_tx(crate::desk::native_input::done)
        .child(text("Done").font(p.ty.label.medium()).color(ink));
    }
    let shown = desk.phone_keyboard_visible();
    touchable(
        el().key("phone-keyboard-toggle")
            .min_w(p.round())
            .min_h(p.round())
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
