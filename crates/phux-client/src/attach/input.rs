//! Stdin VT-byte parsing: turns the bytes a TTY puts on stdin into structured
//! [`InputEvent`]s (SPEC §9). libghostty-vt only ships encoders, so this is a
//! small hand-rolled xterm/VT input lexer.
//!
//! Handled: printable ASCII and UTF-8, C0 controls (CR = Enter, LF = Ctrl-J,
//! Ctrl-letters), ESC+char as Alt, CSI/SS3 cursor, navigation and function
//! keys with xterm modifiers, kitty `CSI u` (press/repeat/release and
//! associated text; hyper folds into SUPER and meta into ALT), SGR / X10 /
//! urxvt-1015 mouse reports (0-indexed cells as integer `f64`, re-quantised
//! by the server's encoder), focus reports, and bracketed paste. DCS / OSC /
//! SOS / PM / APC strings are absorbed and dropped.
//!
//! A bare ESC is ambiguous with the start of a sequence, so the parser stays
//! timer-free: the driver arms an idle timer while [`StdinParser::esc_pending`]
//! and calls [`StdinParser::flush`] when it fires. Partial sequences resume
//! across [`StdinParser::feed`] calls.

use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};

/// Internal parser state machine.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    /// Saw ESC; the next byte picks bare ESC, Alt+char, CSI, SS3, or a string.
    Escape,
    /// Inside `ESC [`, accumulating parameter bytes until a final byte.
    Csi,
    /// Inside `ESC O`; the next byte is the final.
    Ss3,
    /// Inside DCS/OSC/SOS/PM/APC, absorbed until BEL or ST (`ESC \`); `esc`
    /// is set after the ESC of a possible ST.
    StringTerm { esc: bool },
    /// In a UTF-8 sequence with `expected` continuation bytes still to come.
    Utf8 { expected: u8 },
    /// Inside a bracketed-paste payload.
    Paste,
    /// Inside a paste payload, just saw ESC (maybe the close marker).
    PasteEscape,
    /// Inside a paste payload, saw `ESC [`; parameters accumulate in `buf`
    /// until `201~` closes the paste or anything else returns them to it.
    PasteCsi,
    /// Consuming the three raw bytes of a legacy X10 mouse report
    /// (`CSI M Cb Cx Cy`); they are not valid CSI parameter bytes.
    X10Mouse { bytes_seen: u8 },
}

/// Stateful, timer-free parser for stdin bytes. See the module doc.
#[derive(Debug)]
pub struct StdinParser {
    state: State,
    /// In-progress CSI / UTF-8 / X10 bytes, kept across feeds.
    buf: Vec<u8>,
    /// Accumulated bracketed-paste payload.
    paste_buf: Vec<u8>,
}

impl Default for StdinParser {
    fn default() -> Self {
        Self::new()
    }
}

impl StdinParser {
    /// New parser in the ground state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: State::Ground,
            buf: Vec::new(),
            paste_buf: Vec::new(),
        }
    }

    /// Whether a [`flush`](Self::flush) would emit, i.e. a lone ESC is waiting
    /// to be disambiguated. Other partial sequences never flush, so arming the
    /// idle timer for them would only wake the loop for nothing.
    #[must_use]
    pub const fn esc_pending(&self) -> bool {
        matches!(self.state, State::Escape)
    }

    /// Emit a pending lone ESC as the Escape key once stdin has gone idle.
    /// Every other in-progress sequence is kept for the next read.
    pub fn flush(&mut self) -> Vec<InputEvent> {
        let mut out = Vec::new();
        self.flush_into(&mut out);
        out
    }

    /// [`flush`](Self::flush), appending into a caller-owned buffer.
    pub fn flush_into(&mut self, out: &mut Vec<InputEvent>) {
        if matches!(self.state, State::Escape) {
            self.state = State::Ground;
            out.push(InputEvent::Key(make_named_key(
                PhysicalKey::Escape,
                ModSet::empty(),
            )));
        }
    }

    /// Feed `bytes` and return any complete events.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<InputEvent> {
        let mut out = Vec::new();
        self.feed_into(bytes, &mut out);
        out
    }

    /// [`feed`](Self::feed), appending into a caller-owned buffer so the
    /// attach loop allocates nothing per keystroke.
    pub fn feed_into(&mut self, bytes: &[u8], out: &mut Vec<InputEvent>) {
        for &b in bytes {
            self.feed_byte(b, out);
        }
    }

    fn feed_byte(&mut self, b: u8, out: &mut Vec<InputEvent>) {
        match self.state {
            State::Ground => self.feed_ground(b, out),
            State::Escape => self.feed_escape(b, out),
            State::Csi => self.feed_csi(b, out),
            State::Ss3 => self.feed_ss3(b, out),
            State::StringTerm { esc } => self.feed_string_term(b, esc),
            State::Utf8 { expected } => self.feed_utf8(b, expected, out),
            State::Paste => self.feed_paste(b),
            State::PasteEscape => self.feed_paste_escape(b),
            State::PasteCsi => self.feed_paste_csi(b, out),
            State::X10Mouse { bytes_seen } => self.feed_x10_mouse(b, bytes_seen, out),
        }
    }

    fn feed_ground(&mut self, b: u8, out: &mut Vec<InputEvent>) {
        if b == 0x1B {
            self.state = State::Escape;
            return;
        }
        if let Some(ev) = c0_or_ascii_to_key(b) {
            out.push(InputEvent::Key(ev));
            return;
        }
        // UTF-8 multibyte lead?
        if let Some(more) = utf8_continuation_count(b) {
            self.buf.clear();
            self.buf.push(b);
            self.state = State::Utf8 { expected: more };
            return;
        }
        // Stray continuation byte or 0x80..=0xBF without a lead. Drop.
        tracing::trace!(byte = b, "dropping stray byte in ground state");
    }

    fn feed_escape(&mut self, b: u8, out: &mut Vec<InputEvent>) {
        match b {
            b'[' => {
                self.buf.clear();
                self.state = State::Csi;
            }
            b'O' => {
                self.state = State::Ss3;
            }
            // DCS / OSC / SOS / PM / APC — absorb until ST or BEL.
            b'P' | b']' | b'X' | b'^' | b'_' => {
                self.state = State::StringTerm { esc: false };
            }
            0x1B => {
                // ESC ESC — treat the first ESC as a complete Escape key
                // and stay in Escape state for the second.
                out.push(InputEvent::Key(make_named_key(
                    PhysicalKey::Escape,
                    ModSet::empty(),
                )));
                // self.state stays Escape.
            }
            // ESC + printable ASCII / ESC + C0 → Alt-chord. Build the
            // base event for `b` and OR in ALT.
            _ => {
                self.state = State::Ground;
                if let Some(mut ev) = c0_or_ascii_to_key(b) {
                    ev.mods |= ModSet::ALT;
                    // Alt-letter does not produce text on most platforms —
                    // strip the text payload so the server's encoder
                    // builds the bytes from key+mods.
                    if ev.mods.contains(ModSet::CTRL) || !ev.mods.contains(ModSet::SHIFT) {
                        ev.text = None;
                    }
                    out.push(InputEvent::Key(ev));
                } else {
                    tracing::trace!(byte = b, "dropping ESC + unrecognised byte");
                }
            }
        }
    }

    fn feed_csi(&mut self, b: u8, out: &mut Vec<InputEvent>) {
        // CSI structure (ECMA-48):
        //   parameter bytes: 0x30..=0x3F   ('0'..'9' ':' ';' '<' '=' '>' '?')
        //   intermediate bytes: 0x20..=0x2F (' ' .. '/')
        //   final byte: 0x40..=0x7E
        if (0x30..=0x3F).contains(&b) || (0x20..=0x2F).contains(&b) {
            self.buf.push(b);
            // Cap the buffer to bound memory if a misbehaving peer floods
            // parameter bytes without a final. xterm caps at ~200.
            if self.buf.len() > 256 {
                tracing::trace!("dropping over-long CSI sequence");
                self.buf.clear();
                self.state = State::Ground;
            }
            return;
        }
        if (0x40..=0x7E).contains(&b) {
            let final_byte = b;
            let params = std::mem::take(&mut self.buf);
            self.state = State::Ground;
            if final_byte == b'~' && params == b"200" {
                self.paste_buf.clear();
                self.state = State::Paste;
                return;
            }
            // Bare `CSI M` is legacy X10 mouse: three raw bytes follow.
            // urxvt-1015 also ends in `M` but carries numeric params.
            if final_byte == b'M' && params.is_empty() {
                self.buf.clear();
                self.state = State::X10Mouse { bytes_seen: 0 };
                return;
            }
            dispatch_csi(&params, final_byte, out);
            return;
        }
        // Unexpected byte inside a CSI — abort cleanly. xterm-vt100 behavior
        // is to cancel the sequence and return to ground.
        tracing::trace!(byte = b, "aborting CSI on unexpected byte");
        self.buf.clear();
        self.state = State::Ground;
    }

    fn feed_ss3(&mut self, b: u8, out: &mut Vec<InputEvent>) {
        self.state = State::Ground;
        let key = match b {
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
            _ => {
                tracing::trace!(byte = b, "unknown SS3 final byte");
                return;
            }
        };
        out.push(InputEvent::Key(make_named_key(key, ModSet::empty())));
    }

    /// BEL ends the string; so does the byte after an ESC (ST is `ESC \`,
    /// and a malformed terminator ends it just the same).
    const fn feed_string_term(&mut self, b: u8, esc: bool) {
        self.state = if esc || b == 0x07 {
            State::Ground
        } else {
            State::StringTerm { esc: b == 0x1B }
        };
    }

    fn feed_utf8(&mut self, b: u8, expected: u8, out: &mut Vec<InputEvent>) {
        if (b & 0xC0) != 0x80 {
            // Not a valid continuation byte. Drop accumulated bytes and
            // reinterpret `b` from the ground state — that's the
            // recovery path most VT parsers use.
            tracing::trace!(byte = b, "invalid UTF-8 continuation, restarting");
            self.buf.clear();
            self.state = State::Ground;
            self.feed_byte(b, out);
            return;
        }
        self.buf.push(b);
        let remaining = expected - 1;
        if remaining == 0 {
            // Complete codepoint. Decode and emit.
            if let Ok(s) = std::str::from_utf8(&self.buf) {
                let cp = s.chars().next().map(u32::from);
                out.push(InputEvent::Key(KeyEvent {
                    action: KeyAction::Press,
                    key: PhysicalKey::Unidentified,
                    mods: ModSet::empty(),
                    consumed_mods: ModSet::empty(),
                    composing: false,
                    text: Some(s.to_owned()),
                    unshifted_codepoint: cp,
                }));
            } else {
                tracing::trace!("invalid UTF-8 sequence dropped");
            }
            self.buf.clear();
            self.state = State::Ground;
        } else {
            self.state = State::Utf8 {
                expected: remaining,
            };
        }
    }

    /// Inside a bracketed-paste payload. We pass everything through into
    /// `paste_buf` verbatim until an ESC arrives — that *might* be the
    /// closing `ESC [ 201 ~`, or it might be part of the payload (a
    /// pasted ANSI escape is valid). We don't decide until we see the
    /// next bytes.
    fn feed_paste(&mut self, b: u8) {
        if b == 0x1B {
            self.state = State::PasteEscape;
            return;
        }
        self.paste_buf.push(b);
    }

    /// In a paste payload, just saw an ESC. If the next byte is `[` we
    /// might be looking at the close marker; otherwise the ESC was part
    /// of the payload and we restore it before the new byte.
    fn feed_paste_escape(&mut self, b: u8) {
        if b == b'[' {
            self.buf.clear();
            self.state = State::PasteCsi;
            return;
        }
        // Not the close marker — keep the ESC and the new byte in the
        // payload.
        self.paste_buf.push(0x1B);
        self.paste_buf.push(b);
        self.state = State::Paste;
    }

    /// In a paste payload, saw `ESC [`. We accumulate parameter bytes
    /// in `buf` until either:
    /// - the `~` final arrives and `buf == "201"` — emit the paste; or
    /// - any other final / unexpected byte arrives — the `ESC [` was
    ///   part of the payload, so we flush `ESC [` + `buf` + this byte
    ///   back into the paste payload and return to Paste mode.
    fn feed_paste_csi(&mut self, b: u8, out: &mut Vec<InputEvent>) {
        // Parameter / intermediate region.
        if (0x30..=0x3F).contains(&b) || (0x20..=0x2F).contains(&b) {
            self.buf.push(b);
            if self.buf.len() > 16 {
                // Way too many param bytes for our close marker; treat
                // the whole accumulation as payload bytes.
                self.flush_pending_paste_escape();
            }
            return;
        }
        if (0x40..=0x7E).contains(&b) {
            // Closing `CSI 201 ~`?
            if b == b'~' && self.buf == b"201" {
                self.buf.clear();
                let data = std::mem::take(&mut self.paste_buf);
                // A DEC 2004 paste is the user's own clipboard action, the
                // same intent boundary as `phux paste`; untrusted would make
                // the server reject ordinary multiline pastes.
                out.push(InputEvent::Paste(PasteEvent {
                    trust: PasteTrust::Trusted,
                    data,
                }));
                self.state = State::Ground;
                return;
            }
            // Some other CSI inside the paste payload. Flush the buffered
            // CSI prefix back into the paste payload, including the final.
            self.flush_pending_paste_escape();
            self.paste_buf.push(b);
            return;
        }
        // Unexpected byte inside the CSI window — flush and restart paste
        // ingestion with the new byte processed under Paste rules.
        self.flush_pending_paste_escape();
        self.feed_paste(b);
    }

    /// Flush a buffered `ESC [` + parameter bytes back into the paste
    /// payload — used when the accumulated bytes turn out not to be a
    /// close marker, so they were part of the user's paste after all.
    /// Restores the parser to [`State::Paste`].
    fn flush_pending_paste_escape(&mut self) {
        self.paste_buf.push(0x1B);
        self.paste_buf.push(b'[');
        self.paste_buf.extend_from_slice(&self.buf);
        self.buf.clear();
        self.state = State::Paste;
    }

    /// Consume one of the three raw payload bytes (`Cb`, `Cx`, `Cy`) of an X10
    /// mouse report, emitting on the third.
    fn feed_x10_mouse(&mut self, b: u8, bytes_seen: u8, out: &mut Vec<InputEvent>) {
        self.buf.push(b);
        let next = bytes_seen + 1;
        if next < 3 {
            self.state = State::X10Mouse { bytes_seen: next };
            return;
        }
        let [cb, cx, cy] = [self.buf[0], self.buf[1], self.buf[2]].map(u32::from);
        self.buf.clear();
        self.state = State::Ground;
        // X10 offsets all three values by 32; urxvt-1015 only the button.
        dispatch_legacy_mouse(cb, cx.saturating_sub(0x20), cy.saturating_sub(0x20), out);
    }
}

/// Map a single byte to a [`KeyEvent`] for the printable / C0 region.
/// Returns `None` for bytes the parser handles elsewhere (ESC, UTF-8
/// continuations, ...).
fn c0_or_ascii_to_key(b: u8) -> Option<KeyEvent> {
    match b {
        // Printable ASCII.
        0x20..=0x7E => Some(KeyEvent {
            action: KeyAction::Press,
            key: ascii_to_physical(b),
            mods: ascii_shift_mods(b),
            consumed_mods: ascii_shift_mods(b),
            composing: false,
            text: Some(char::from(b).to_string()),
            unshifted_codepoint: Some(u32::from(ascii_unshifted(b))),
        }),
        // CR → Enter. LF (0x0A) is Ctrl+J: it falls through to the Ctrl-letter
        // arm below so the server encoder reproduces 0x0A rather than CR. In raw
        // mode the host TTY sends CR for Return and LF only for Ctrl+J, so this
        // split is unambiguous.
        0x0D => Some(make_named_key(PhysicalKey::Enter, ModSet::empty())),
        // BS / DEL → Backspace.
        0x08 | 0x7F => Some(make_named_key(PhysicalKey::Backspace, ModSet::empty())),
        // HT → Tab.
        0x09 => Some(make_named_key(PhysicalKey::Tab, ModSet::empty())),
        // Ctrl-A..Ctrl-Z (skipping the dedicated mappings above and ESC). LF
        // (0x0A) lands here as Ctrl+J → letter 'J'.
        0x01..=0x1A if b != 0x08 && b != 0x09 && b != 0x0D => {
            let letter = b'A' + (b - 1);
            Some(KeyEvent {
                action: KeyAction::Press,
                key: letter_key(letter),
                mods: ModSet::CTRL,
                consumed_mods: ModSet::CTRL,
                composing: false,
                text: None,
                unshifted_codepoint: Some(u32::from(letter.to_ascii_lowercase())),
            })
        }
        _ => None,
    }
}

/// Build a key event for a "named" key (no text payload).
#[must_use]
pub const fn make_named_key(key: PhysicalKey, mods: ModSet) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods,
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    }
}

/// Map a printable ASCII byte to a [`PhysicalKey`]. Punctuation goes through
/// [`phux_config::keybind::punct_to_key`] so it matches the key the chord
/// parser builds for the same glyph, or punctuation keybinds never fire.
const fn ascii_to_physical(b: u8) -> PhysicalKey {
    match b {
        b' ' => PhysicalKey::Space,
        b'0'..=b'9' => match b {
            b'0' => PhysicalKey::Digit0,
            b'1' => PhysicalKey::Digit1,
            b'2' => PhysicalKey::Digit2,
            b'3' => PhysicalKey::Digit3,
            b'4' => PhysicalKey::Digit4,
            b'5' => PhysicalKey::Digit5,
            b'6' => PhysicalKey::Digit6,
            b'7' => PhysicalKey::Digit7,
            b'8' => PhysicalKey::Digit8,
            _ => PhysicalKey::Digit9,
        },
        b'A'..=b'Z' | b'a'..=b'z' => letter_key(b.to_ascii_uppercase()),
        _ => match phux_config::keybind::punct_to_key(b as char) {
            Some((key, _shift)) => key,
            None => PhysicalKey::Unidentified,
        },
    }
}

/// `SHIFT` for uppercase letters and shifted punctuation, which also goes in
/// `consumed_mods` (SPEC §9.1.3) so the encoder does not apply it twice.
const fn ascii_shift_mods(b: u8) -> ModSet {
    if ascii_unshifted(b) == b {
        ModSet::empty()
    } else {
        ModSet::SHIFT
    }
}

/// The unmodified glyph for `unshifted_codepoint` (`A` -> `a`, `@` -> `2`),
/// assuming US QWERTY.
const fn ascii_unshifted(b: u8) -> u8 {
    match b {
        b'A'..=b'Z' => b + 32,
        b'!' => b'1',
        b'@' => b'2',
        b'#' => b'3',
        b'$' => b'4',
        b'%' => b'5',
        b'^' => b'6',
        b'&' => b'7',
        b'*' => b'8',
        b'(' => b'9',
        b')' => b'0',
        b'_' => b'-',
        b'+' => b'=',
        b'{' => b'[',
        b'}' => b']',
        b'|' => b'\\',
        b':' => b';',
        b'"' => b'\'',
        b'<' => b',',
        b'>' => b'.',
        b'?' => b'/',
        b'~' => b'`',
        _ => b,
    }
}

/// How many continuation bytes follow this byte in a UTF-8 sequence.
/// Returns `None` for non-lead bytes (ASCII or stray continuations).
const fn utf8_continuation_count(b: u8) -> Option<u8> {
    match b {
        0xC2..=0xDF => Some(1),
        0xE0..=0xEF => Some(2),
        0xF0..=0xF4 => Some(3),
        _ => None,
    }
}

/// Dispatch a finished CSI sequence into [`InputEvent`]s.
///
/// `params` is the parameter / intermediate region (everything between
/// `ESC [` and the final byte); `final_byte` is the final byte
/// (`0x40..=0x7E`).
fn dispatch_csi(params: &[u8], final_byte: u8, out: &mut Vec<InputEvent>) {
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
    let mods = match parsed {
        [1, code, ..] => xterm_modifier_code(*code),
        _ => ModSet::empty(),
    };
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

/// Kitty modifier code (`1 + bitfield`) to [`ModSet`]. Bits are shift, alt,
/// ctrl, super, hyper, meta, caps lock, num lock; `ModSet` has no hyper or
/// meta, so they fold into SUPER and ALT.
fn kitty_modifier_code(code: u32) -> ModSet {
    const BITS: [ModSet; 8] = [
        ModSet::SHIFT,
        ModSet::ALT,
        ModSet::CTRL,
        ModSet::SUPER,
        ModSet::SUPER,
        ModSet::ALT,
        ModSet::CAPS_LOCK,
        ModSet::NUM_LOCK,
    ];
    let bits = code.saturating_sub(1);
    BITS.iter()
        .enumerate()
        .filter(|(bit, _)| bits & (1 << bit) != 0)
        .fold(ModSet::empty(), |mods, (_, m)| mods | *m)
}

/// Map a kitty keycode to a [`PhysicalKey`]: ASCII letters, digits and a few
/// controls by value, functional keys by their kitty PUA codepoints, other
/// printable codepoints as `Unidentified` (the codepoint still travels in
/// `unshifted_codepoint`).
#[allow(
    clippy::too_many_lines,
    reason = "flat keycode table — the size IS the spec"
)]
const fn kitty_keycode_to_physical(cp: u32) -> Option<PhysicalKey> {
    Some(match cp {
        27 => PhysicalKey::Escape,
        13 => PhysicalKey::Enter,
        9 => PhysicalKey::Tab,
        127 => PhysicalKey::Backspace,
        #[allow(clippy::cast_possible_truncation, reason = "range-checked u32 -> u8")]
        c @ (0x20 | 0x30..=0x39 | 0x41..=0x5A | 0x61..=0x7A) => ascii_to_physical(c as u8),
        // Functional keys (kitty PUA range).
        57348 => PhysicalKey::Insert,
        57349 => PhysicalKey::Delete,
        57350 => PhysicalKey::ArrowLeft,
        57351 => PhysicalKey::ArrowRight,
        57352 => PhysicalKey::ArrowUp,
        57353 => PhysicalKey::ArrowDown,
        57354 => PhysicalKey::PageUp,
        57355 => PhysicalKey::PageDown,
        57356 => PhysicalKey::Home,
        57357 => PhysicalKey::End,
        57358 => PhysicalKey::CapsLock,
        57359 => PhysicalKey::ScrollLock,
        57360 => PhysicalKey::NumLock,
        57361 => PhysicalKey::PrintScreen,
        57362 => PhysicalKey::Pause,
        57363 => PhysicalKey::ContextMenu,
        57364 => PhysicalKey::F1,
        57365 => PhysicalKey::F2,
        57366 => PhysicalKey::F3,
        57367 => PhysicalKey::F4,
        57368 => PhysicalKey::F5,
        57369 => PhysicalKey::F6,
        57370 => PhysicalKey::F7,
        57371 => PhysicalKey::F8,
        57372 => PhysicalKey::F9,
        57373 => PhysicalKey::F10,
        57374 => PhysicalKey::F11,
        57375 => PhysicalKey::F12,
        57376 => PhysicalKey::F13,
        57377 => PhysicalKey::F14,
        57378 => PhysicalKey::F15,
        57379 => PhysicalKey::F16,
        57380 => PhysicalKey::F17,
        57381 => PhysicalKey::F18,
        57382 => PhysicalKey::F19,
        57383 => PhysicalKey::F20,
        57384 => PhysicalKey::F21,
        57385 => PhysicalKey::F22,
        57386 => PhysicalKey::F23,
        57387 => PhysicalKey::F24,
        57388 => PhysicalKey::F25,
        // Numpad 0..9.
        57399 => PhysicalKey::Numpad0,
        57400 => PhysicalKey::Numpad1,
        57401 => PhysicalKey::Numpad2,
        57402 => PhysicalKey::Numpad3,
        57403 => PhysicalKey::Numpad4,
        57404 => PhysicalKey::Numpad5,
        57405 => PhysicalKey::Numpad6,
        57406 => PhysicalKey::Numpad7,
        57407 => PhysicalKey::Numpad8,
        57408 => PhysicalKey::Numpad9,
        57409 => PhysicalKey::NumpadDecimal,
        57410 => PhysicalKey::NumpadDivide,
        57411 => PhysicalKey::NumpadMultiply,
        57412 => PhysicalKey::NumpadSubtract,
        57413 => PhysicalKey::NumpadAdd,
        57414 => PhysicalKey::NumpadEnter,
        57415 => PhysicalKey::NumpadEqual,
        57416 => PhysicalKey::NumpadSeparator,
        57417 => PhysicalKey::NumpadLeft,
        57418 => PhysicalKey::NumpadRight,
        57419 => PhysicalKey::NumpadUp,
        57420 => PhysicalKey::NumpadDown,
        57421 => PhysicalKey::NumpadPageUp,
        57422 => PhysicalKey::NumpadPageDown,
        57423 => PhysicalKey::NumpadHome,
        57424 => PhysicalKey::NumpadEnd,
        57425 => PhysicalKey::NumpadInsert,
        57426 => PhysicalKey::NumpadDelete,
        57427 => PhysicalKey::NumpadBegin,
        // Modifier keys.
        57441 => PhysicalKey::ShiftLeft,
        57442 => PhysicalKey::ControlLeft,
        57443 => PhysicalKey::AltLeft,
        57444 => PhysicalKey::MetaLeft,
        57447 => PhysicalKey::ShiftRight,
        57448 => PhysicalKey::ControlRight,
        57449 => PhysicalKey::AltRight,
        57450 => PhysicalKey::MetaRight,
        _ if cp >= 0x20 => PhysicalKey::Unidentified,
        _ => return None,
    })
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
fn dispatch_legacy_mouse(btn: u32, col: u32, row: u32, out: &mut Vec<InputEvent>) {
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
        b'Z' => PhysicalKey::Tab, // CSI Z = Shift-Tab; modifier filled by caller
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

/// The [`PhysicalKey`] for an uppercase ASCII letter.
const fn letter_key(b: u8) -> PhysicalKey {
    match b {
        b'A' => PhysicalKey::A,
        b'B' => PhysicalKey::B,
        b'C' => PhysicalKey::C,
        b'D' => PhysicalKey::D,
        b'E' => PhysicalKey::E,
        b'F' => PhysicalKey::F,
        b'G' => PhysicalKey::G,
        b'H' => PhysicalKey::H,
        b'I' => PhysicalKey::I,
        b'J' => PhysicalKey::J,
        b'K' => PhysicalKey::K,
        b'L' => PhysicalKey::L,
        b'M' => PhysicalKey::M,
        b'N' => PhysicalKey::N,
        b'O' => PhysicalKey::O,
        b'P' => PhysicalKey::P,
        b'Q' => PhysicalKey::Q,
        b'R' => PhysicalKey::R,
        b'S' => PhysicalKey::S,
        b'T' => PhysicalKey::T,
        b'U' => PhysicalKey::U,
        b'V' => PhysicalKey::V,
        b'W' => PhysicalKey::W,
        b'X' => PhysicalKey::X,
        b'Y' => PhysicalKey::Y,
        b'Z' => PhysicalKey::Z,
        _ => PhysicalKey::Unidentified,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn is_ground(p: &StdinParser) -> bool {
        p.state == State::Ground
    }

    /// Feed `bytes` to a fresh parser, which must yield exactly one event and
    /// end back in the ground state.
    fn one_event(bytes: &[u8]) -> InputEvent {
        let mut p = StdinParser::new();
        let mut evs = p.feed(bytes);
        assert_eq!(evs.len(), 1, "input {bytes:?} produced {evs:?}");
        assert!(
            is_ground(&p),
            "input {bytes:?} left the parser mid-sequence"
        );
        evs.remove(0)
    }

    fn one_key(bytes: &[u8]) -> KeyEvent {
        match one_event(bytes) {
            InputEvent::Key(key) => key,
            other => panic!("input {bytes:?}: expected a key, got {other:?}"),
        }
    }

    fn one_mouse(bytes: &[u8]) -> MouseEvent {
        match one_event(bytes) {
            InputEvent::Mouse(mouse) => mouse,
            other => panic!("input {bytes:?}: expected a mouse event, got {other:?}"),
        }
    }

    fn one_paste(bytes: &[u8]) -> PasteEvent {
        match one_event(bytes) {
            InputEvent::Paste(paste) => paste,
            other => panic!("input {bytes:?}: expected a paste, got {other:?}"),
        }
    }

    #[test]
    fn key_sequences_decode_to_key_and_mods() {
        let shift_alt_ctrl = ModSet::SHIFT | ModSet::ALT | ModSet::CTRL;
        let cases: &[(&[u8], PhysicalKey, ModSet)] = &[
            (b"a", PhysicalKey::A, ModSet::empty()),
            (b"A", PhysicalKey::A, ModSet::SHIFT),
            (b"-", PhysicalKey::Minus, ModSet::empty()),
            (b"|", PhysicalKey::Backslash, ModSet::SHIFT),
            (b"\r", PhysicalKey::Enter, ModSet::empty()),
            (b"\t", PhysicalKey::Tab, ModSet::empty()),
            (b"\x7f", PhysicalKey::Backspace, ModSet::empty()),
            (b"\x03", PhysicalKey::C, ModSet::CTRL),
            (b"\x02", PhysicalKey::B, ModSet::CTRL),
            // LF is Ctrl-J, not Enter: the encoder would otherwise send CR.
            (b"\n", PhysicalKey::J, ModSet::CTRL),
            (b"\x1ba", PhysicalKey::A, ModSet::ALT),
            (b"\x1b[A", PhysicalKey::ArrowUp, ModSet::empty()),
            (b"\x1b[1;5A", PhysicalKey::ArrowUp, ModSet::CTRL),
            (b"\x1b[1;8C", PhysicalKey::ArrowRight, shift_alt_ctrl),
            (b"\x1b[1;9D", PhysicalKey::ArrowLeft, ModSet::SUPER),
            // A private marker is not differentiated; a first param other
            // than 1 carries no modifier.
            (b"\x1b[>1;5A", PhysicalKey::ArrowUp, ModSet::CTRL),
            (b"\x1b[2;5A", PhysicalKey::ArrowUp, ModSet::empty()),
            (b"\x1b[H", PhysicalKey::Home, ModSet::empty()),
            (b"\x1b[1~", PhysicalKey::Home, ModSet::empty()),
            (b"\x1b[5~", PhysicalKey::PageUp, ModSet::empty()),
            (b"\x1b[5;5~", PhysicalKey::PageUp, ModSet::CTRL),
            (b"\x1b[15~", PhysicalKey::F5, ModSet::empty()),
            (b"\x1b[17~", PhysicalKey::F6, ModSet::empty()),
            (b"\x1b[18~", PhysicalKey::F7, ModSet::empty()),
            (b"\x1b[19~", PhysicalKey::F8, ModSet::empty()),
            (b"\x1b[20~", PhysicalKey::F9, ModSet::empty()),
            (b"\x1b[21~", PhysicalKey::F10, ModSet::empty()),
            (b"\x1b[23~", PhysicalKey::F11, ModSet::empty()),
            (b"\x1b[24~", PhysicalKey::F12, ModSet::empty()),
            // SS3, including `ESC O P`, which must not read as a focus report.
            (b"\x1bOP", PhysicalKey::F1, ModSet::empty()),
            (b"\x1bOQ", PhysicalKey::F2, ModSet::empty()),
            (b"\x1bOR", PhysicalKey::F3, ModSet::empty()),
            (b"\x1bOS", PhysicalKey::F4, ModSet::empty()),
            (b"\x1bOA", PhysicalKey::ArrowUp, ModSet::empty()),
            // Kitty `CSI u`.
            (b"\x1b[97u", PhysicalKey::A, ModSet::empty()),
            (b"\x1b[97;5u", PhysicalKey::A, ModSet::CTRL),
            (b"\x1b[97;8u", PhysicalKey::A, shift_alt_ctrl),
            (b"\x1b[97;9u", PhysicalKey::A, ModSet::SUPER),
            (b"\x1b[97;17u", PhysicalKey::A, ModSet::SUPER),
            (b"\x1b[97;33u", PhysicalKey::A, ModSet::ALT),
            (b"\x1b[97;65u", PhysicalKey::A, ModSet::CAPS_LOCK),
            (b"\x1b[97;129u", PhysicalKey::A, ModSet::NUM_LOCK),
            (b"\x1b[97:65:97;2u", PhysicalKey::A, ModSet::SHIFT),
            (b"\x1b[57368u", PhysicalKey::F5, ModSet::empty()),
            (b"\x1b[57352u", PhysicalKey::ArrowUp, ModSet::empty()),
            (b"\x1b[27u", PhysicalKey::Escape, ModSet::empty()),
            (b"\x1b[13u", PhysicalKey::Enter, ModSet::empty()),
        ];
        for &(input, key, mods) in cases {
            let ev = one_key(input);
            assert_eq!((ev.key, ev.mods), (key, mods), "input {input:?}");
        }
    }

    #[test]
    fn printable_keys_carry_text_and_consumed_shift() {
        let lower = one_key(b"a");
        assert_eq!(lower.text.as_deref(), Some("a"));
        assert_eq!(lower.action, KeyAction::Press);
        let upper = one_key(b"A");
        assert!(upper.consumed_mods.contains(ModSet::SHIFT));
        assert_eq!(upper.unshifted_codepoint, Some(u32::from('a')));
        assert_eq!(one_key(b"\x1ba").text, None, "Alt-letter carries no text");
    }

    /// Punctuation must decode to the same (key, shift) the chord parser
    /// builds for the glyph, or default binds like `"|"` never fire.
    #[test]
    fn punctuation_matches_chord_parser() {
        for &glyph in b"|-`=[]\\;',./~!@#$%^&*()_+{}:\"<>?" {
            let ev = one_key(&[glyph]);
            let chord = phux_config::keybind::parse_chord(&char::from(glyph).to_string())
                .expect("chord parse");
            assert_eq!(
                (ev.key, ev.mods.contains(ModSet::SHIFT)),
                (chord.key, chord.modifiers.contains(ModSet::SHIFT)),
                "mismatch on glyph {}",
                char::from(glyph)
            );
        }
    }

    #[test]
    fn utf8_decodes_and_recovers_from_a_bad_continuation() {
        let e_acute = one_key(&[0xC3, 0xA9]);
        assert_eq!(e_acute.text.as_deref(), Some("é"));
        assert_eq!(e_acute.unshifted_codepoint, Some(0x00E9));
        assert_eq!(one_key("😀".as_bytes()).text.as_deref(), Some("😀"));
        // A lead byte followed by ASCII drops the lead and keeps the ASCII.
        assert_eq!(one_key(&[0xC3, b'a']).text.as_deref(), Some("a"));
    }

    #[test]
    fn kitty_event_types_and_associated_text() {
        let actions: &[(&[u8], KeyAction)] = &[
            (b"\x1b[97;1:1u", KeyAction::Press),
            (b"\x1b[97;1:2u", KeyAction::Repeat),
            (b"\x1b[97;1:3u", KeyAction::Release),
        ];
        for &(input, action) in actions {
            assert_eq!(one_key(input).action, action, "input {input:?}");
        }
        let texts: &[(&[u8], Option<&str>)] = &[
            (b"\x1b[97;1:1:97u", Some("a")),
            (b"\x1b[97;1:1:97:769u", Some("a\u{0301}")),
            (b"\x1b[97;1:1:0u", None),
            (b"\x1b[57364;1:1:120u", Some("x")),
        ];
        for &(input, text) in texts {
            assert_eq!(one_key(input).text.as_deref(), text, "input {input:?}");
        }
        assert_eq!(one_key(b"\x1b[97u").unshifted_codepoint, Some(97));
    }

    #[test]
    fn mouse_reports_decode() {
        use MouseAction::{Motion, Press, Release};
        use MouseButton::{Five, Four, Left, Middle, Right, Unknown};
        let shift_alt_ctrl = ModSet::SHIFT | ModSet::ALT | ModSet::CTRL;
        #[allow(clippy::type_complexity, reason = "test table")]
        let cases: &[(&[u8], MouseAction, MouseButton, ModSet, f64, f64)] = &[
            // SGR: 1-indexed cells become 0-indexed coordinates.
            (b"\x1b[<0;5;3M", Press, Left, ModSet::empty(), 4.0, 2.0),
            (b"\x1b[<0;5;3m", Release, Left, ModSet::empty(), 4.0, 2.0),
            (b"\x1b[<2;1;1M", Press, Right, ModSet::empty(), 0.0, 0.0),
            (b"\x1b[<1;1;1M", Press, Middle, ModSet::empty(), 0.0, 0.0),
            (b"\x1b[<32;10;5M", Motion, Left, ModSet::empty(), 9.0, 4.0),
            (b"\x1b[<35;1;1M", Motion, Unknown, ModSet::empty(), 0.0, 0.0),
            (b"\x1b[<64;1;1M", Press, Four, ModSet::empty(), 0.0, 0.0),
            (b"\x1b[<65;1;1M", Press, Five, ModSet::empty(), 0.0, 0.0),
            (b"\x1b[<28;1;1M", Press, Left, shift_alt_ctrl, 0.0, 0.0),
            // X10 raw bytes: every value offset by 32; a release names no button.
            (
                b"\x1b[M\x20\x21\x21",
                Press,
                Left,
                ModSet::empty(),
                0.0,
                0.0,
            ),
            (
                b"\x1b[M\x23\x25\x23",
                Release,
                Unknown,
                ModSet::empty(),
                4.0,
                2.0,
            ),
            (b"\x1b[M\x24\x21\x21", Press, Left, ModSet::SHIFT, 0.0, 0.0),
            // A high payload byte is data, not a new sequence.
            (
                b"\x1b[M\x20\x21\xff",
                Press,
                Left,
                ModSet::empty(),
                0.0,
                222.0,
            ),
            // urxvt-1015: decimal params, button offset by 32.
            (b"\x1b[32;5;3M", Press, Left, ModSet::empty(), 4.0, 2.0),
            (b"\x1b[35;5;3M", Release, Unknown, ModSet::empty(), 4.0, 2.0),
            (b"\x1b[96;1;1M", Press, Four, ModSet::empty(), 0.0, 0.0),
        ];
        for &(input, action, button, mods, x, y) in cases {
            let ev = one_mouse(input);
            assert_eq!(
                (ev.action, ev.button, ev.mods),
                (action, button, mods),
                "{input:?}"
            );
            assert!((ev.x - x).abs() < f64::EPSILON && (ev.y - y).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn focus_reports_decode() {
        for (input, expected) in [
            (b"\x1b[I", FocusEvent::Gained),
            (b"\x1b[O", FocusEvent::Lost),
        ] {
            assert!(matches!(one_event(input), InputEvent::Focus(f) if f == expected));
        }
    }

    #[test]
    fn bracketed_paste_keeps_its_payload_verbatim() {
        for payload in [
            b"hello world".as_slice(),
            b"red\x1b[31mthing\x1b[0mend",
            b"a\x1bb",
        ] {
            let mut framed = b"\x1b[200~".to_vec();
            framed.extend_from_slice(payload);
            framed.extend_from_slice(b"\x1b[201~");
            let paste = one_paste(&framed);
            assert_eq!(paste.data, payload);
            assert_eq!(paste.trust, PasteTrust::Trusted);
        }
    }

    /// A read boundary can fall anywhere; splitting must not change the events.
    #[test]
    fn every_read_split_decodes_like_one_read() {
        let inputs: &[&[u8]] = &[
            b"\x1b[1;5A",
            b"\x1b[<0;5;3M",
            b"\x1b[M\x20\x21\x21",
            b"\x1b[97;1:1:97:769u",
            "😀".as_bytes(),
            b"\x1b[200~if true; then\r\n\tprintf 'hi'\n\nfi\n\x1b[201~",
        ];
        for input in inputs {
            let whole = StdinParser::new().feed(input);
            for split in 1..input.len() {
                let mut p = StdinParser::new();
                let mut evs = p.feed(&input[..split]);
                evs.extend(p.feed(&input[split..]));
                assert_eq!(evs, whole, "input {input:?} split at {split}");
                assert!(is_ground(&p));
            }
        }
    }

    #[test]
    fn malformed_or_unknown_sequences_drop_and_recover() {
        // An over-long CSI is abandoned; the parser must not stay stuck in it.
        let mut overlong = b"\x1b[".to_vec();
        overlong.extend(std::iter::repeat_n(b'1', 300));
        let mut p = StdinParser::new();
        p.feed(&overlong);
        assert!(is_ground(&p));

        for input in [
            b"\x1b[1;2z".as_slice(),
            b"\x1b[u",
            // A two-param urxvt-shaped report, an unknown `~` key, and a
            // focus letter carrying params all drop.
            b"\x1b[5;3M",
            b"\x1b[99~",
            b"\x1b[1I",
            b"\x1b]0;title\x07",
            b"\x1bPdata\x1b\\",
        ] {
            let mut p = StdinParser::new();
            assert!(p.feed(input).is_empty(), "input {input:?}");
            assert!(is_ground(&p), "input {input:?}");
        }
    }

    #[test]
    fn a_lone_esc_waits_for_flush_and_only_it_arms_the_timer() {
        let mut p = StdinParser::new();
        assert!(!p.esc_pending());
        assert!(p.feed(b"\x1b").is_empty(), "bare ESC must wait for flush");
        assert!(p.esc_pending());
        let flushed = p.flush();
        assert!(matches!(&flushed[..], [InputEvent::Key(k)] if k.key == PhysicalKey::Escape));
        assert!(is_ground(&p));

        // ESC ESC: the first is a complete Escape, the second still waits.
        let evs = p.feed(b"\x1b\x1b");
        assert!(matches!(&evs[..], [InputEvent::Key(k)] if k.key == PhysicalKey::Escape));
        assert!(p.esc_pending());
        // ... and a following byte turns it into an Alt chord.
        let evs = p.feed(b"x");
        assert!(matches!(&evs[..], [InputEvent::Key(k)] if k.mods == ModSet::ALT));

        // A partial CSI neither arms the timer nor yields to a flush.
        assert!(p.feed(b"\x1b[1;").is_empty());
        assert!(!p.esc_pending());
        assert!(p.flush().is_empty());
        assert_eq!(p.feed(b"5A").len(), 1);
    }

    /// The attach loop feeds one retained buffer: `feed_into` and `flush_into`
    /// must append, or a batch loses every event before the last read.
    #[test]
    fn feed_into_and_flush_into_append() {
        let mut p = StdinParser::new();
        let mut out = Vec::new();
        p.feed_into(b"ab", &mut out);
        p.feed_into(b"c\x1b", &mut out);
        p.flush_into(&mut out);
        assert_eq!(out.len(), 4);
        assert!(matches!(&out[3], InputEvent::Key(k) if k.key == PhysicalKey::Escape));
    }
}
