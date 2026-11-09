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
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};

mod csi;
mod key;

use self::csi::{dispatch_csi, dispatch_legacy_mouse};
use self::key::{c0_or_ascii_to_key, utf8_continuation_count};
pub use key::make_named_key;

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

#[cfg(test)]
mod tests;
