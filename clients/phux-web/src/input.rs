//! Browser keyboard, IME, paste, and mouse routing, free of the DOM so it runs
//! under the node test harness.
//!
//! The browser delivers terminal input three ways: a `keydown` for a key the
//! terminal encodes itself, committed text (an IME composition, a dead-key
//! sequence, a mobile keyboard's `insertText`) that arrives without a usable
//! keydown, and a `paste`. [`route_key`] decides which keydowns are terminal
//! input; the rest are left to the browser so it can compose text, paste, or
//! run its own shortcut.

use std::ops::RangeInclusive;

use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::MouseButton;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};

pub use phux_client_core::keys::key_events_for_text;

/// Largest clipboard payload sent as one `INPUT_PASTE`. Half the outbound
/// transport budget, so a paste never overflows it and tears the connection
/// down; a larger clipboard is refused locally instead.
pub const MAX_PASTE_BYTES: usize = 512 * 1024;

/// The fields of a browser `KeyboardEvent` that routing reads.
#[derive(Clone, Copy, Debug, Default)]
pub struct BrowserKey<'a> {
    /// `KeyboardEvent.key`: the produced character or the named key.
    pub key: &'a str,
    /// `KeyboardEvent.code`: the physical key.
    pub code: &'a str,
    /// `ctrlKey`.
    pub ctrl: bool,
    /// `shiftKey`.
    pub shift: bool,
    /// `altKey` (Option on macOS).
    pub alt: bool,
    /// `metaKey` (Command on macOS, the Windows/Super key elsewhere).
    pub meta: bool,
    /// `getModifierState("AltGraph")`: `AltGr` chords report Ctrl+Alt on
    /// Windows while producing a character.
    pub alt_graph: bool,
    /// `repeat`: the key is auto-repeating.
    pub repeat: bool,
    /// `isComposing`: an IME composition owns this keystroke.
    pub composing: bool,
}

/// Named keys that only modify another key and carry no input themselves.
const MODIFIER_KEYS: [&str; 12] = [
    "Shift",
    "Control",
    "Alt",
    "AltGraph",
    "Meta",
    "OS",
    "Super",
    "Hyper",
    "CapsLock",
    "NumLock",
    "ScrollLock",
    "Fn",
];

/// Route one `keydown`: `Some` is a terminal key event the caller sends and
/// then cancels the browser default for; `None` leaves the keystroke to the
/// browser.
///
/// The browser keeps: IME and dead-key keystrokes (the composed text arrives
/// later as committed text), keys it cannot identify (mobile keyboards, whose
/// text arrives through `input`), bare modifiers, Command/Super chords (copy,
/// paste, find, reload, and the rest of the platform's shortcuts, as the
/// desktop client does), and the Ctrl+Shift+V and Shift+Insert paste chords,
/// so the browser raises a `paste` event.
#[must_use]
pub fn route_key(key: &BrowserKey<'_>) -> Option<KeyEvent> {
    if key.composing || matches!(key.key, "Process" | "Dead" | "Unidentified" | "") {
        return None;
    }
    if MODIFIER_KEYS.contains(&key.key) || key.meta || is_paste_chord(key) {
        return None;
    }

    // AltGr reports Ctrl+Alt on Windows; the chord is spent producing the
    // character, so neither reaches the terminal as a modifier.
    let (ctrl, alt) = if key.alt_graph {
        (false, false)
    } else {
        (key.ctrl, key.alt)
    };
    let mut mods = ModSet::empty();
    if ctrl {
        mods |= ModSet::CTRL;
    }
    if key.shift {
        mods |= ModSet::SHIFT;
    }
    if alt {
        mods |= ModSet::ALT;
    }

    let text = (!ctrl && produces_text(key.key)).then(|| key.key.to_owned());
    let mut consumed_mods = ModSet::empty();
    if text.is_some() {
        // Shift chose the character. A non-ASCII character typed with Alt
        // is macOS Option composing (Option+a is "å"): Alt was spent too,
        // so the server must not also prefix ESC. Alt with an ASCII
        // character stays a Meta chord (readline's Alt+b).
        consumed_mods = mods & ModSet::SHIFT;
        if alt && !key.key.is_ascii() {
            consumed_mods |= ModSet::ALT;
        }
    }

    Some(KeyEvent {
        action: if key.repeat {
            KeyAction::Repeat
        } else {
            KeyAction::Press
        },
        key: code_to_physical_key(key.code),
        mods,
        consumed_mods,
        composing: false,
        text,
        unshifted_codepoint: None,
    })
}

/// Whether a `KeyboardEvent.key` value is produced text rather than a named
/// key. Named keys (`Enter`, `ArrowUp`, `F1`) are multi-letter ASCII words;
/// a layout may produce a multi-scalar string, which is never pure ASCII.
fn produces_text(key: &str) -> bool {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => !c.is_control(),
        (Some(_), Some(_)) => !key.is_ascii(),
        (None, _) => false,
    }
}

/// Ctrl+Shift+V and Shift+Insert: the paste chords of terminals without a
/// Command key. The browser raises `paste` for them when not cancelled.
fn is_paste_chord(key: &BrowserKey<'_>) -> bool {
    let ctrl_shift_v = key.ctrl && key.shift && !key.alt && key.code == "KeyV";
    let shift_insert = key.shift && !key.ctrl && !key.alt && key.code == "Insert";
    ctrl_shift_v || shift_insert
}

/// Command+C, or Ctrl+Shift+C where there is no Command key: copy the
/// selection, when there is one.
#[must_use]
pub fn is_copy_chord(key: &BrowserKey<'_>) -> bool {
    let command_c = key.meta && !key.ctrl && !key.alt && key.code == "KeyC";
    let ctrl_shift_c = key.ctrl && key.shift && !key.alt && !key.meta && key.code == "KeyC";
    command_c || ctrl_shift_c
}

/// Command+F, or Ctrl+Shift+F where there is no Command key: find in the
/// terminal. Only a keystroke aimed at the terminal reaches this, so the
/// browser keeps its own find everywhere else on the page.
#[must_use]
pub fn is_find_chord(key: &BrowserKey<'_>) -> bool {
    let command_f = key.meta && !key.ctrl && !key.alt && !key.shift && key.code == "KeyF";
    let ctrl_shift_f = key.ctrl && key.shift && !key.alt && !key.meta && key.code == "KeyF";
    command_f || ctrl_shift_f
}

/// The wire button for a DOM `MouseEvent.button`: 0 primary, 1 middle,
/// 2 secondary, 3 back, 4 forward (xterm's buttons 8 and 9). `None` for
/// `-1` (no button changed: a move) and anything else.
#[must_use]
pub const fn mouse_button(button: i16) -> Option<MouseButton> {
    Some(match button {
        0 => MouseButton::Left,
        1 => MouseButton::Middle,
        2 => MouseButton::Right,
        3 => MouseButton::Eight,
        4 => MouseButton::Nine,
        _ => return None,
    })
}

/// The held button a drag reports, from a DOM `MouseEvent.buttons` mask
/// (1 primary, 2 secondary, 4 middle, 8 back, 16 forward): the lowest set
/// bit, or `None` when nothing is held.
#[must_use]
pub const fn held_button(buttons: u16) -> Option<MouseButton> {
    Some(match buttons.isolate_lowest_one() {
        1 => MouseButton::Left,
        2 => MouseButton::Right,
        4 => MouseButton::Middle,
        8 => MouseButton::Eight,
        16 => MouseButton::Nine,
        _ => return None,
    })
}

/// The wheel "button" xterm reports for scrolling `rows` rows: 4 up, 5 down.
#[must_use]
pub const fn wheel_button(rows: i32) -> MouseButton {
    if rows < 0 {
        MouseButton::Four
    } else {
        MouseButton::Five
    }
}

/// Rows of wheel travel that make one reported wheel click: programs scroll
/// about three lines per click, so a mouse notch reads the same in a
/// program as in the local scrollback.
pub const WHEEL_ROWS_PER_CLICK: f64 = 3.0;

/// One axis of a pointer position as `INPUT_MOUSE` carries it: whole pixels
/// of the cell grid the viewport reports, `cell_px` per cell, clamped to
/// `count` cells. `offset` is the pointer's distance from the canvas edge
/// and `css_cell` a cell's drawn size, both in CSS pixels, so CSS scaling,
/// page zoom, and the device pixel ratio change neither which cell nor
/// where in it the program sees the pointer.
#[must_use]
pub fn surface_pixel(offset: f64, css_cell: f64, cell_px: u16, count: u16) -> f64 {
    let cells = offset / css_cell.max(f64::EPSILON);
    let max = (f64::from(count) * f64::from(cell_px) - 1.0).max(0.0);
    (cells * f64::from(cell_px)).floor().clamp(0.0, max)
}

/// The program modes a wheel event is routed by, read off the replica.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WheelModes {
    /// The program tracks the mouse (DECSET 9, 1000, 1002, or 1003).
    pub tracking: bool,
    /// The program is on the alternate screen (DECSET 47, 1047, or 1049).
    pub alt_screen: bool,
    /// Alternate scroll (DECSET 1007) is on: the engine's default, until
    /// the program clears it.
    pub alt_scroll: bool,
}

/// What a wheel event over the terminal does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelAction {
    /// Page the local scrollback.
    Scrollback,
    /// Report wheel presses (buttons 4 and 5) to a mouse-tracking program.
    Report,
    /// Send arrow keys, one per row, to a program on the alternate screen.
    Arrows,
}

/// Route one wheel event, as the reference TUI does: a mouse-tracking
/// program gets the wheel; a program on the alternate screen, which has no
/// scrollback, gets it as arrow keys under alternate scroll (xterm's
/// `alternateScroll`); otherwise it pages the local scrollback. Shift, and a
/// view already scrolled back, keep it local.
#[must_use]
pub const fn route_wheel(modes: WheelModes, shift: bool, scrolled: bool) -> WheelAction {
    if shift || scrolled {
        WheelAction::Scrollback
    } else if modes.tracking {
        WheelAction::Report
    } else if modes.alt_screen && modes.alt_scroll {
        WheelAction::Arrows
    } else {
        WheelAction::Scrollback
    }
}

/// The arrow-key presses for `rows` rows of wheel travel: Up for negative
/// (toward history), Down for positive, unmodified, so the server encodes
/// them in the program's cursor-key mode.
#[must_use]
pub fn wheel_arrows(rows: i32) -> Vec<KeyEvent> {
    let key = if rows < 0 {
        PhysicalKey::ArrowUp
    } else {
        PhysicalKey::ArrowDown
    };
    let press = KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    };
    vec![press; rows.unsigned_abs() as usize]
}

/// Whether pointer motion reaches the program: any-event tracking (DECSET
/// 1003) reports every move, button-event tracking (1002) only drags, and
/// normal tracking (1000) none.
#[must_use]
pub const fn reports_motion(any_event: bool, button_event: bool, button_held: bool) -> bool {
    any_event || (button_event && button_held)
}

/// Shift+PageUp / Shift+PageDown page the local scrollback (as in ghostty
/// and xterm) instead of reaching the terminal: `-1` pages up, `1` down.
#[must_use]
pub fn scrollback_page(key: &BrowserKey<'_>) -> Option<i32> {
    if !key.shift || key.ctrl || key.alt || key.meta || key.composing {
        return None;
    }
    match key.code {
        "PageUp" => Some(-1),
        "PageDown" => Some(1),
        _ => None,
    }
}

/// `WheelEvent.deltaMode` values.
const WHEEL_PIXELS: u32 = 0;
const WHEEL_LINES: u32 = 1;

/// Whole rows a wheel event scrolls (negative is up, into scrollback).
/// Pixel deltas accumulate in `carry` across events, so a trackpad's many
/// small deltas still add up to rows; line deltas are rows, page deltas
/// are `page_rows`.
pub fn wheel_rows(delta_y: f64, mode: u32, row_px: f64, page_rows: u16, carry: &mut f64) -> i32 {
    let rows = match mode {
        WHEEL_PIXELS => delta_y / row_px.max(1.0),
        WHEEL_LINES => delta_y,
        _ => delta_y * f64::from(page_rows.max(1)),
    };
    *carry += rows;
    let whole = carry.trunc();
    *carry -= whole;
    whole as i32
}

/// The `INPUT_PASTE` payload for clipboard text, or `None` when there is
/// nothing to paste or the payload exceeds [`MAX_PASTE_BYTES`].
///
/// A browser paste is the user's own clipboard action, the same intent
/// boundary as the TUI's bracketed paste and `phux paste`, so it is
/// `Trusted`: untrusted would make the server refuse every multiline paste.
/// The server still brackets it from the pane's DEC 2004 state.
#[must_use]
pub fn paste_event(text: &str) -> Option<PasteEvent> {
    if text.is_empty() || text.len() > MAX_PASTE_BYTES {
        return None;
    }
    Some(PasteEvent {
        trust: PasteTrust::Trusted,
        data: text.as_bytes().to_vec(),
    })
}

/// Map a W3C `KeyboardEvent.code` to libghostty's physical-key discriminant.
#[must_use]
pub fn code_to_physical_key(code: &str) -> PhysicalKey {
    contiguous_key(code).unwrap_or_else(|| named_key(code))
}

/// Keys whose discriminants run contiguously from a base (ADR-0024):
/// `KeyA` = 20, `Digit0` = 6, `Numpad0` = 80, `F1` = 121.
fn contiguous_key(code: &str) -> Option<PhysicalKey> {
    let (base, index) = if let Some(rest) = code.strip_prefix("Key") {
        (20, single(rest, 'A'..='Z')?)
    } else if let Some(rest) = code.strip_prefix("Digit") {
        (6, single(rest, '0'..='9')?)
    } else if let Some(rest) = code.strip_prefix("Numpad") {
        (80, single(rest, '0'..='9')?)
    } else {
        (121, function_index(code.strip_prefix('F')?)?)
    };
    PhysicalKey::try_from(base + index).ok()
}

/// The offset within `range` of a one-character suffix.
fn single(suffix: &str, range: RangeInclusive<char>) -> Option<u32> {
    let mut chars = suffix.chars();
    let c = chars.next().filter(|c| range.contains(c))?;
    chars
        .next()
        .is_none()
        .then(|| c as u32 - *range.start() as u32)
}

/// `F1`..=`F25` as `0..=24`; `F0`, `F01`, and `Fn` are not function keys.
fn function_index(suffix: &str) -> Option<u32> {
    let n: u32 = suffix.parse().ok().filter(|_| !suffix.starts_with('0'))?;
    (1..=25).contains(&n).then(|| n - 1)
}

fn named_key(code: &str) -> PhysicalKey {
    use PhysicalKey as K;

    match code {
        "Enter" | "NumpadEnter" => K::Enter,
        "Backspace" => K::Backspace,
        "Tab" => K::Tab,
        "Space" => K::Space,
        "Escape" => K::Escape,
        "ArrowUp" => K::ArrowUp,
        "ArrowDown" => K::ArrowDown,
        "ArrowLeft" => K::ArrowLeft,
        "ArrowRight" => K::ArrowRight,
        "Home" => K::Home,
        "End" => K::End,
        "PageUp" => K::PageUp,
        "PageDown" => K::PageDown,
        "Delete" => K::Delete,
        "Insert" => K::Insert,
        "Minus" => K::Minus,
        "Equal" => K::Equal,
        "Period" => K::Period,
        "Comma" => K::Comma,
        "Slash" => K::Slash,
        "Semicolon" => K::Semicolon,
        "Quote" => K::Quote,
        "Backslash" => K::Backslash,
        "BracketLeft" => K::BracketLeft,
        "BracketRight" => K::BracketRight,
        "Backquote" => K::Backquote,
        "IntlBackslash" => K::IntlBackslash,
        "IntlRo" => K::IntlRo,
        "IntlYen" => K::IntlYen,
        "ContextMenu" => K::ContextMenu,
        "Help" => K::Help,
        "PrintScreen" => K::PrintScreen,
        "Pause" => K::Pause,
        "NumpadAdd" => K::NumpadAdd,
        "NumpadSubtract" => K::NumpadSubtract,
        "NumpadMultiply" => K::NumpadMultiply,
        "NumpadDivide" => K::NumpadDivide,
        "NumpadDecimal" => K::NumpadDecimal,
        "NumpadComma" => K::NumpadComma,
        "NumpadEqual" => K::NumpadEqual,
        _ => K::Unidentified,
    }
}
