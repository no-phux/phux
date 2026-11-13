//! A chord (Ctrl+V, Ctrl+Shift+C, ...) on the virtual keyboard, pressed the
//! way a real keyboard does: the modifier keys go down first, then the
//! letter. On Hyprland a mask sent alone, before any key of this keyboard
//! made it the active one, left chords without effect while plain typing
//! worked: the key press switched keyboards and the client's fresh keymap
//! state had no Control.

use super::Op;

/// The Shift and Control bits of a modifier mask: xkb's first real
/// modifiers, the same in every keymap.
const SHIFT: u32 = 1;
const CONTROL: u32 = 1 << 2;
/// xkb keycodes (evdev + 8) of Left Control and Left Shift.
const LCTL: u32 = 29 + 8;
const LFSH: u32 = 42 + 8;

/// Ctrl (with `shift`, Ctrl+Shift) and `letter`, on a keymap of the real
/// keys: `Control_L` and `Shift_L` mapped to their modifiers, and the letter.
pub(in super::super) fn chord(letter: char, shift: bool) -> Op {
    let key = super::super::surface_input::letter_code(letter);
    let code = key + 8;
    let upper = letter.to_ascii_uppercase();
    let keymap = format!(
        "xkb_keymap {{\nxkb_keycodes \"phux\" {{\nminimum = 8;\nmaximum = {max};\n\
         <LCTL> = {LCTL};\n<LFSH> = {LFSH};\n<K> = {code};\n}};\n\
         xkb_types \"phux\" {{ include \"complete\" }};\n\
         xkb_compatibility \"phux\" {{ include \"complete\" }};\n\
         xkb_symbols \"phux\" {{\nkey <LCTL> {{[Control_L]}};\nkey <LFSH> {{[Shift_L]}};\n\
         key <K> {{[{letter}, {upper}]}};\n\
         modifier_map Control {{ <LCTL> }};\nmodifier_map Shift {{ <LFSH> }};\n}};\n}};\n",
        max = code.max(LFSH),
    );
    let mut held = vec![LCTL - 8];
    if shift {
        held.push(LFSH - 8);
    }
    Op::Keys {
        keymap,
        codes: vec![key],
        mods: CONTROL | if shift { SHIFT } else { 0 },
        held,
    }
}
