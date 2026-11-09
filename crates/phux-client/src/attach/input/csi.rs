//! Decode completed CSI keyboard, focus, and mouse reports.
use super::key::{kitty_keycode_to_physical, kitty_modifier_code, make_named_key};
use phux_protocol::input::{
    InputEvent,
    focus::FocusEvent,
    key::{KeyAction, KeyEvent, ModSet, PhysicalKey},
    mouse::{MouseAction, MouseButton, MouseEvent},
};

/// Dispatch a finished CSI sequence into [`InputEvent`]s.
///
/// `params` is the parameter / intermediate region (everything between
/// `ESC [` and the final byte); `final_byte` is the final byte
/// (`0x40..=0x7E`).
pub(super) fn dispatch_csi(params: &[u8], final_byte: u8, out: &mut Vec<InputEvent>) {
    // SGR mouse (DEC 1006): `CSI < btn ; col ; row M|m`.
    if is_sgr_mouse_report(params, final_byte) {
        dispatch_sgr_mouse(&params[1..], final_byte, out);
        return;
    }

    // urxvt-1015 mouse: `CSI btn ; col ; row M`, plain decimal params only.
    if let Some([btn, col, row]) = urxvt_mouse_params(params, final_byte) {
        dispatch_legacy_mouse(btn, col, row, out);
        return;
    }

    if final_byte == b'u' {
        dispatch_kitty_csi_u(params, out);
        return;
    }

    // Private markers are not differentiated.
    let body = strip_private_marker(params);

    // Focus reports (DEC 1004) are bare `CSI I` / `CSI O`.
    if body.is_empty()
        && let Some(focus) = focus_report(final_byte)
    {
        out.push(InputEvent::Focus(focus));
        return;
    }

    let parsed = parse_csi_params(body);
    if final_byte == b'~' {
        dispatch_csi_tilde(&parsed, out);
    } else {
        dispatch_csi_letter(&parsed, final_byte, out);
    }
}

/// A CSI parameter region's leading private marker byte.
const fn is_private_marker(b: u8) -> bool {
    matches!(b, b'?' | b'<' | b'=' | b'>')
}

/// `params` without its leading private marker, if any.
const fn strip_private_marker(params: &[u8]) -> &[u8] {
    match params.split_first() {
        Some((&first, rest)) if is_private_marker(first) => rest,
        _ => params,
    }
}

fn is_sgr_mouse_report(params: &[u8], final_byte: u8) -> bool {
    params.first() == Some(&b'<') && matches!(final_byte, b'M' | b'm')
}

/// The `btn ; col ; row` of a urxvt-1015 mouse report: final `M`, no
/// private marker, plain decimal parameters, and exactly three of them.
fn urxvt_mouse_params(params: &[u8], final_byte: u8) -> Option<[u32; 3]> {
    if final_byte != b'M' || params.first().is_some_and(|&b| is_private_marker(b)) {
        return None;
    }
    if !params
        .iter()
        .all(|&b| matches!(b, b'0'..=b'9' | b';' | b':'))
    {
        return None;
    }
    parse_csi_params(params).try_into().ok()
}

const fn focus_report(final_byte: u8) -> Option<FocusEvent> {
    match final_byte {
        b'I' => Some(FocusEvent::Gained),
        b'O' => Some(FocusEvent::Lost),
        _ => None,
    }
}

/// `CSI n ; mod ~`: navigation and function keys.
fn dispatch_csi_tilde(parsed: &[u32], out: &mut Vec<InputEvent>) {
    // xterm modifyOtherKeys form `CSI 27 ; mod ; keycode ~`. Ghostty sends it
    // in legacy mode for modified Enter/Tab/Backspace (Cmd+Return is
    // `CSI 27;9;13~`); decode it like the kitty `CSI keycode;mod u` it mirrors.
    if let [27, code, keycode] = *parsed {
        let Some(key) = kitty_keycode_to_physical(keycode) else {
            tracing::trace!(keycode, "modifyOtherKeys unmapped keycode");
            return;
        };
        out.push(InputEvent::Key(KeyEvent {
            unshifted_codepoint: Some(keycode),
            ..make_named_key(key, xterm_modifier_code(code))
        }));
        return;
    }
    let n = parsed.first().copied().unwrap_or(1);
    let mods = parsed
        .get(1)
        .copied()
        .map_or(ModSet::empty(), xterm_modifier_code);
    let Some(key) = csi_tilde_keycode(n) else {
        tracing::trace!(n, final_byte = b'~', "unknown CSI ~ keycode");
        return;
    };
    out.push(InputEvent::Key(make_named_key(key, mods)));
}

/// `CSI 1 ; mod letter` or bare `CSI letter`: arrows, Home/End, F1-F4.
fn dispatch_csi_letter(parsed: &[u32], final_byte: u8, out: &mut Vec<InputEvent>) {
    let Some(key) = csi_letter_keycode(final_byte) else {
        tracing::trace!(final_byte, ?parsed, "unknown CSI sequence");
        return;
    };
    let mut mods = match parsed {
        [1, code, ..] => xterm_modifier_code(*code),
        _ => ModSet::empty(),
    };
    if final_byte == b'Z' {
        mods |= ModSet::SHIFT;
    }
    out.push(InputEvent::Key(make_named_key(key, mods)));
}

/// Decode an SGR mouse report body (`btn ; col ; row`, after the `<`).
/// `btn` uses the xterm bitfield: low bits select the button, 4/8/16 are
/// Shift/Alt/Ctrl, 32 is motion, 64 wheel, 128 extra buttons. The final
/// byte `m` marks a release.
fn dispatch_sgr_mouse(body: &[u8], final_byte: u8, out: &mut Vec<InputEvent>) {
    let parsed = parse_csi_params(body);
    let [raw_btn, col, row, ..] = parsed[..] else {
        tracing::trace!(?parsed, "malformed SGR mouse report (too few params)");
        return;
    };
    let action = if (raw_btn & 0x20) != 0 {
        MouseAction::Motion
    } else if final_byte == b'm' {
        MouseAction::Release
    } else {
        MouseAction::Press
    };
    push_mouse(action, raw_btn, col, row, out);
}

/// Push a mouse event from an xterm button code and 1-indexed cell position,
/// as 0-indexed integer-valued `f64` coordinates (SPEC §9.2.1).
fn push_mouse(action: MouseAction, raw_btn: u32, col: u32, row: u32, out: &mut Vec<InputEvent>) {
    out.push(InputEvent::Mouse(MouseEvent {
        action,
        button: sgr_mouse_button(raw_btn),
        mods: sgr_mouse_mods(raw_btn),
        x: f64::from(col.saturating_sub(1)),
        y: f64::from(row.saturating_sub(1)),
    }));
}

/// Decode a kitty keyboard-protocol sequence,
/// `CSI keycode[:shifted:base][;mods[:event_type[:text...]]] u`.
/// The level-3 alternate keys are dropped; nonzero text codepoints become
/// [`KeyEvent::text`].
fn dispatch_kitty_csi_u(params: &[u8], out: &mut Vec<InputEvent>) {
    let groups = parse_csi_param_groups(params);
    let keycode_group = groups.first();
    let keycode = keycode_group.and_then(|g| g.first().copied()).unwrap_or(0);
    if keycode == 0 {
        tracing::trace!("kitty CSI u with empty keycode, dropping");
        return;
    }

    let mod_group = groups.get(1);
    let raw_mod = mod_group.and_then(|g| g.first().copied()).unwrap_or(1);
    let event_type = mod_group.and_then(|g| g.get(1).copied()).unwrap_or(1);

    let Some(key) = kitty_keycode_to_physical(keycode) else {
        tracing::trace!(keycode, "kitty CSI u unmapped keycode");
        return;
    };

    out.push(InputEvent::Key(KeyEvent {
        action: kitty_key_action(event_type),
        key,
        mods: kitty_modifier_code(raw_mod),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: mod_group.and_then(|g| kitty_text(g.get(2..)?)),
        unshifted_codepoint: Some(keycode),
    }));
}

/// Kitty event-type sub-parameter to [`KeyAction`]; unknown is a press.
const fn kitty_key_action(event_type: u32) -> KeyAction {
    match event_type {
        3 => KeyAction::Release,
        2 => KeyAction::Repeat,
        _ => KeyAction::Press,
    }
}

/// The text carried by kitty's associated-text codepoints, skipping zero
/// and invalid codepoints; `None` when nothing remains.
fn kitty_text(codepoints: &[u32]) -> Option<String> {
    let mut text = String::new();
    for &cp in codepoints.iter().filter(|&&cp| cp != 0) {
        if let Some(c) = char::from_u32(cp) {
            text.push(c);
        } else {
            // Typed text: never log the value (ADR-0028).
            tracing::trace!("kitty CSI u: invalid text codepoint, skipping");
        }
    }
    (!text.is_empty()).then_some(text)
}

/// Parse CSI parameter bytes into groups: `:` separates sub-parameters within
/// a group, `;` (or any other non-digit) starts a new group. Empty slots are 0.
fn parse_csi_param_groups(body: &[u8]) -> Vec<Vec<u32>> {
    let mut groups = vec![Vec::new()];
    let mut acc: u32 = 0;
    for &b in body {
        if b.is_ascii_digit() {
            acc = acc.saturating_mul(10).saturating_add(u32::from(b - b'0'));
            continue;
        }
        if let Some(group) = groups.last_mut() {
            group.push(acc);
        }
        acc = 0;
        if b != b':' {
            groups.push(Vec::new());
        }
    }
    if let Some(group) = groups.last_mut() {
        group.push(acc);
    }
    groups
}

/// Parse CSI parameters as a flat list, treating every separator alike.
fn parse_csi_params(body: &[u8]) -> Vec<u32> {
    parse_csi_param_groups(body).concat()
}

/// Shift/Alt/Ctrl bits (4/8/16) of an xterm mouse button code.
fn sgr_mouse_mods(raw: u32) -> ModSet {
    kitty_modifier_code(1 + ((raw >> 2) & 0b111))
}

/// The button an xterm mouse code names; low bits `3` mean no button.
const fn sgr_mouse_button(raw: u32) -> MouseButton {
    if raw & 0x40 != 0 {
        return match raw & 0x03 {
            0 => MouseButton::Four,  // wheel up
            1 => MouseButton::Five,  // wheel down
            2 => MouseButton::Six,   // wheel left
            _ => MouseButton::Seven, // wheel right
        };
    }
    if raw & 0x80 != 0 {
        return match raw & 0x03 {
            0 => MouseButton::Eight,
            1 => MouseButton::Nine,
            2 => MouseButton::Ten,
            _ => MouseButton::Eleven,
        };
    }
    match raw & 0x03 {
        0 => MouseButton::Left,
        1 => MouseButton::Middle,
        2 => MouseButton::Right,
        _ => MouseButton::Unknown,
    }
}

/// Decode an X10 / urxvt-1015 mouse report: button code offset by 32, 1-indexed
/// cell position. Neither has a release final byte; a release is low bits `3`
/// (no button), except on wheel / extra-button codes, which are presses.
pub(super) fn dispatch_legacy_mouse(btn: u32, col: u32, row: u32, out: &mut Vec<InputEvent>) {
    let raw_btn = btn.saturating_sub(0x20);
    let is_wheel_or_extra = (raw_btn & 0xC0) != 0;
    let action = if (raw_btn & 0x20) != 0 {
        MouseAction::Motion
    } else if !is_wheel_or_extra && (raw_btn & 0x03) == 0x03 {
        MouseAction::Release
    } else {
        MouseAction::Press
    };
    push_mouse(action, raw_btn, col, row, out);
}

/// xterm modifier code (`1 + shift|alt|ctrl|super`): the kitty encoding's low
/// four bits.
fn xterm_modifier_code(code: u32) -> ModSet {
    kitty_modifier_code(1 + (code.saturating_sub(1) & 0b1111))
}

/// `CSI <letter>` keycodes.
const fn csi_letter_keycode(final_byte: u8) -> Option<PhysicalKey> {
    Some(match final_byte {
        b'A' => PhysicalKey::ArrowUp,
        b'B' => PhysicalKey::ArrowDown,
        b'C' => PhysicalKey::ArrowRight,
        b'D' => PhysicalKey::ArrowLeft,
        b'F' => PhysicalKey::End,
        b'H' => PhysicalKey::Home,
        b'P' => PhysicalKey::F1,
        b'Q' => PhysicalKey::F2,
        b'R' => PhysicalKey::F3,
        b'S' => PhysicalKey::F4,
        b'Z' => PhysicalKey::Tab, // CSI Z = Shift-Tab; dispatch_csi_letter adds Shift
        _ => return None,
    })
}

/// `CSI <n> ~` keycodes per the VT220 / xterm conventions.
const fn csi_tilde_keycode(n: u32) -> Option<PhysicalKey> {
    Some(match n {
        1 | 7 => PhysicalKey::Home,
        2 => PhysicalKey::Insert,
        3 => PhysicalKey::Delete,
        4 | 8 => PhysicalKey::End,
        5 => PhysicalKey::PageUp,
        6 => PhysicalKey::PageDown,
        11 | 15 => PhysicalKey::F5, // 11=F1 on linux-console but xterm uses 15=F5; either-or
        13 => PhysicalKey::F3,
        14 => PhysicalKey::F4,
        17 => PhysicalKey::F6,
        18 => PhysicalKey::F7,
        19 => PhysicalKey::F8,
        20 => PhysicalKey::F9,
        21 => PhysicalKey::F10,
        23 => PhysicalKey::F11,
        24 => PhysicalKey::F12,
        25 => PhysicalKey::F13,
        26 => PhysicalKey::F14,
        28 => PhysicalKey::F15,
        29 => PhysicalKey::F16,
        31 => PhysicalKey::F17,
        32 => PhysicalKey::F18,
        33 => PhysicalKey::F19,
        34 => PhysicalKey::F20,
        _ => return None,
    })
}
