//! Stdin VT-byte parsing for the attach loop.
//!
//! The parser turns the byte stream a TTY puts on stdin into structured
//! [`InputEvent`]s aimed at SPEC §9 input frames. It is deliberately small
//! and local — libghostty-vt ships *encoders* (event → bytes) but not a
//! decoder for the reverse direction, so this module hand-rolls just
//! enough of an xterm/VT100 input parser to cover what `phux attach` sees
//! in practice.
//!
//! # Surface area
//!
//! Handled:
//!
//! - Printable ASCII and full UTF-8 multibyte sequences (Unicode keypresses)
//! - C0 controls — CR/LF (Enter), HT (Tab), BS / DEL (Backspace), and the
//!   Ctrl-A..Ctrl-Z encoding (`0x01..=0x1A` minus the dedicated mappings).
//! - **Bare ESC** as the Escape key (see "Timing ambiguity" below).
//! - **ESC + char** as an Alt-modified key (Alt-letter chords).
//! - **CSI** sequences (`ESC [` …) for arrow keys, Home/End/Insert/Delete,
//!   PageUp/PageDown, F-keys via the xterm `CSI 1;<mod>P..S` and VT220
//!   `CSI <n>~` forms, and modifier-bearing variants
//!   (`CSI 1;5A` for Ctrl-Up, `CSI 5;5~` for Ctrl-PageUp, etc.).
//! - **SS3** sequences (`ESC O <P|Q|R|S>`) for the older F1..F4 + numeric
//!   keypad encoding most BSD `termcap` entries still use.
//!
//! Also handled:
//!
//! - **SGR mouse reports** (`CSI < <btn> ; <col> ; <row> M/m`, DEC mode
//!   1006). Emitted as [`InputEvent::Mouse`] with terminal-local cell
//!   coordinates expressed as integer-valued `f64` pixels (per SPEC §9.2.1
//!   the encoder downstream re-quantises for cell-format mouse protocols).
//! - **Legacy X10 mouse** (`CSI M Cb Cx Cy`) — three raw bytes after the
//!   `M` final encode button + position (`Cb = btn + 32`,
//!   `Cx = col + 32`, `Cy = row + 32`). This form cannot be parameterised
//!   as a normal CSI numeric sequence, so the parser drops into a
//!   dedicated 3-byte consumer state (`State::X10Mouse`) when a bare
//!   `CSI M` (empty params) is observed. X10 has no separate release
//!   final byte; releases are signalled by `(Cb & 3) == 3`, which maps
//!   to [`MouseAction::Release`] with [`MouseButton::Unknown`] (matching
//!   the legacy protocol's inability to identify which button was
//!   released).
//! - **urxvt-1015 decimal mouse** (`CSI <btn> ; <col> ; <row> M`). Same
//!   button bitfield as X10 (offset by 32) but with decimal parameters
//!   instead of raw bytes, and always terminated by `M`. Distinguished
//!   from SGR (DEC 1006) by the absence of the leading `<` private-marker.
//! - **Focus reports** (`CSI I` / `CSI O`, DEC mode 1004). Emitted as
//!   [`InputEvent::Focus`].
//! - **Bracketed paste** (`CSI 200~` … `CSI 201~`, DEC mode 2004). The
//!   parser buffers payload bytes between the begin / end markers and
//!   emits a single [`InputEvent::Paste`] at the end-marker. Payload
//!   bytes are passed through verbatim — no nested escape parsing.
//!
//! Not handled (yet):
//!
//! - **Kitty keyboard protocol `CSI u`** sequences — disambiguate-escape
//!   (KIP level 1), event-types including repeat (KIP level 2), and partial
//!   levels 3-5. Sequences of the form
//!   `CSI keycode[:shifted_key:base_layout_key][;modifiers[:event_type[:text_codepoints]]] u`
//!   decode into [`KeyEvent`]s. We surface press / release / repeat as
//!   distinct [`KeyAction`] variants; we drop the level-3 `shifted_key` /
//!   `base_layout_key` sub-parameters (no encoder integration yet); we
//!   decode the level-5 text codepoints into `KeyEvent.text` whenever
//!   they're present. Hyper / meta modifier bits are collapsed into
//!   `SUPER` / `ALT` respectively — libghostty's `Mods` lacks distinct
//!   hyper / meta bits and a phux-side wrapper is out of scope for v0.
//!   See <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>.
//! - DCS / OSC / SOS / PM / APC sequences inbound — these come from the
//!   inner program, not from a keyboard, and have no representation as a
//!   `KeyEvent`. They are absorbed and dropped.
//!
//! # libghostty surface check
//!
//! libghostty-vt exposes input *encoders* (`key::Encoder`,
//! `mouse::Encoder`, `focus::Event::encode`, `paste::encode`) but no
//! parser in the reverse direction — there is no `Decoder`, no
//! `parse_bytes`, no public state machine equivalent to xterm's VT input
//! lexer. Confirmed by grepping the `libghostty-vt` crate at the pinned
//! rev (`31d1f70`); only the `terminal::Terminal::vt_write` parser
//! exists, and that is for output bytes from a child process, not input
//! bytes from a host terminal. This module therefore hand-rolls the
//! CSI / SS3 lexer required for inbound input parsing.
//!
//! # Timing ambiguity (bare ESC)
//!
//! Bare ESC and ESC-starting-a-sequence are not distinguishable from the
//! byte stream alone. The classic xterm-style solution is a short idle
//! timeout: if ESC is followed by another byte within ~50ms it starts a
//! sequence, otherwise it stands alone as the Escape key.
//!
//! This module exposes that decision to the driver via [`StdinParser::flush`].
//! The driver arms a [`tokio::time::Sleep`] whenever
//! [`StdinParser::has_pending`] returns `true` and calls `flush()` if the
//! sleep wins the next `select!` race. That keeps timing policy out of
//! the parser (which is sync and timer-free) and inside the loop that
//! already knows about wall time.
//!
//! # Partial sequences across read boundaries
//!
//! A single CSI sequence can straddle two `read()` calls; the parser
//! holds its in-progress state (a private `State` enum + a small
//! parameter buffer) in the [`StdinParser`] struct and resumes on the
//! next [`StdinParser::feed`] call.

use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};

// This module parses stdin VT bytes into the wire-level
// [`phux_protocol::input::InputEvent`] directly — the parser yields
// `Key` from real input bytes plus the `Mouse` / `Focus` / `Paste`
// variants populated by the mouse / focus / paste parsing paths.

/// Internal parser state machine.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Default; consuming plain bytes.
    #[default]
    Ground,
    /// Just saw an ESC; the next byte decides whether this is bare ESC,
    /// Alt+char, CSI, SS3, DCS, or something we drop.
    Escape,
    /// Inside a CSI sequence (`ESC [` …), accumulating parameter / intermediate
    /// bytes until a final byte in `0x40..=0x7E` arrives.
    Csi,
    /// Inside an SS3 sequence (`ESC O` …); next byte is the final.
    Ss3,
    /// Inside a string-like control (DCS, OSC, SOS, PM, APC). We absorb
    /// until ST (`ESC \`) or BEL.
    StringTerm,
    /// In a UTF-8 multibyte continuation; `expected` more continuation
    /// bytes remain.
    Utf8 { expected: u8 },
    /// Inside a bracketed-paste payload; bytes accumulate in `paste_buf`
    /// until the closing `ESC [ 201 ~` marker is seen.
    Paste,
    /// Inside a paste payload, just saw an ESC. The next bytes might be
    /// the closing `[ 201 ~` marker, or they might be part of the paste
    /// payload (e.g. a nested ANSI sequence the user pasted). We parse
    /// minimally enough to spot the end marker.
    PasteEscape,
    /// Inside a paste payload, saw `ESC [`. Accumulating the (numeric)
    /// parameter bytes in `buf` until either a final byte arrives that
    /// completes the `CSI 201~` close marker, or anything else, in which
    /// case the accumulated bytes are flushed back into the paste payload.
    PasteCsi,
    /// Consuming the 3 trailing bytes (`Cb`, `Cx`, `Cy`) of a legacy X10
    /// mouse report (`CSI M Cb Cx Cy`). The X10 mouse format is *not* a
    /// numeric-parameterised CSI sequence — the three bytes after the
    /// `M` final are raw single-byte values, including bytes that would
    /// otherwise be invalid CSI parameter bytes. We therefore enter a
    /// dedicated consumer state when bare `CSI M` (empty params) is
    /// observed.
    ///
    /// `bytes_seen` counts how many of the three payload bytes have been
    /// stored in [`StdinParser::buf`]. The report completes — and emits
    /// an event — when the third byte arrives.
    X10Mouse { bytes_seen: u8 },
}

/// Stateful parser for stdin bytes.
///
/// The parser is timer-free; the driver arms a `tokio::time::Sleep` and
/// calls [`StdinParser::flush`] when stdin has been idle for the bare-ESC
/// timeout. See the module doc.
#[derive(Debug)]
pub struct StdinParser {
    state: State,
    /// In-progress CSI / SS3 / UTF-8 / string-term bytes. Reused across
    /// feeds so partial sequences resume cleanly.
    buf: Vec<u8>,
    /// Pending UTF-8 lead byte (when `state == Utf8`).
    utf8_lead: u8,
    /// Accumulated bytes of an in-progress bracketed-paste payload.
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
            utf8_lead: 0,
            paste_buf: Vec::new(),
        }
    }

    /// Returns `true` if the parser is currently inside an in-progress
    /// sequence, i.e. mid-decode of *something*.
    ///
    /// Note this is broader than "a flush would emit": see
    /// [`esc_pending`](Self::esc_pending) for the predicate the idle timer
    /// actually wants.
    #[must_use]
    pub const fn has_pending(&self) -> bool {
        !matches!(self.state, State::Ground)
    }

    /// Returns `true` only when a [`flush`](Self::flush) would emit an event —
    /// that is, when a lone ESC is waiting to be disambiguated.
    ///
    /// This is the predicate the driver arms the bare-ESC idle timer from.
    /// [`has_pending`](Self::has_pending) is true for every in-progress
    /// sequence, and for all of them but `State::Escape` the flush is
    /// documented to *keep* the state and emit nothing — so arming on it
    /// bought a guaranteed-useless 10ms wake-up (plus a whole empty dispatch
    /// batch) in the middle of every multi-byte key that happened to land
    /// split across a read boundary. A complete `CSI`/`SS3` sequence already
    /// emits the instant its final byte arrives and never waited; narrowing
    /// the arming to the one state that *can* produce an event changes no
    /// output, only how often the loop wakes up for nothing (phux-l96p.4).
    #[must_use]
    pub const fn esc_pending(&self) -> bool {
        matches!(self.state, State::Escape)
    }

    /// Flush pending parser state on a timing boundary.
    ///
    /// Called by the driver when stdin has been idle long enough that a
    /// bare ESC can no longer be the prefix of a sequence. Effects:
    ///
    /// - `State::Escape`: emit a single Escape key and return to ground.
    /// - All other in-progress states are *kept* — a CSI half-sent over
    ///   the network is not "abandoned" just because of an idle gap; the
    ///   next read is still expected to complete it. (Real terminals
    ///   never split CSI sequences across long idle windows.)
    pub fn flush(&mut self) -> Vec<InputEvent> {
        let mut out = Vec::new();
        self.flush_into(&mut out);
        out
    }

    /// [`flush`](Self::flush), appending into a caller-owned buffer.
    ///
    /// The attach loop keeps one buffer for the life of the attach so the
    /// keystroke path allocates nothing per event batch.
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
    ///
    /// The parser does not allocate when no events come out beyond the
    /// returned `Vec` itself. Hot callers should prefer
    /// [`feed_into`](Self::feed_into), which reuses the caller's buffer.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<InputEvent> {
        let mut out = Vec::new();
        self.feed_into(bytes, &mut out);
        out
    }

    /// [`feed`](Self::feed), appending into a caller-owned buffer.
    ///
    /// `feed` allocated a fresh `Vec` per stdin read; on the attach loop that
    /// is one allocation per keystroke for a container that is emptied again
    /// microseconds later. Feeding into a retained buffer makes the steady
    /// state allocation-free (phux-l96p.4).
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
            State::StringTerm => self.feed_string_term(b),
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
            self.utf8_lead = b;
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
                self.state = State::StringTerm;
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
            // Move buf out so dispatch_csi can borrow self immutably.
            let params = std::mem::take(&mut self.buf);
            self.state = State::Ground;
            // Bracketed-paste begin (`CSI 200~`) puts us into Paste state
            // instead of emitting an event; everything else goes through
            // the normal CSI dispatch.
            if final_byte == b'~' && params == b"200" {
                self.paste_buf.clear();
                self.state = State::Paste;
                return;
            }
            // Legacy X10 mouse (`CSI M Cb Cx Cy`): bare `CSI M` with no
            // parameter bytes. The next 3 bytes are raw button + position
            // and must be consumed in a dedicated state — they are not
            // valid CSI parameter bytes (they can be anything in `0x20..`).
            // urxvt-1015 also terminates with `M`, but always carries
            // semicolon-separated numeric params, so it falls through to
            // `dispatch_csi` below.
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

    fn feed_string_term(&mut self, b: u8) {
        // We track ST (`ESC \`) by re-entering State::Escape so the next
        // `\` finishes the string. BEL (0x07) also terminates an OSC.
        if b == 0x07 {
            self.state = State::Ground;
        } else if b == 0x1B {
            // Could be ST. We re-use State::Escape to find the next byte;
            // the only "exit" path from a string-terminator ESC is `\`,
            // anything else is a malformed sequence which we also drop.
            // For simplicity, just return to ground unconditionally on the
            // next byte by treating any byte after this ESC as "consumed
            // and we're done". Use a dedicated marker by stashing a
            // sentinel — but to keep state minimal, just consume the next
            // byte in StringTerm-end mode via Escape and bounce out.
            //
            // Simplest robust thing: leave State::StringTerm, the next
            // byte will be processed as the string-terminator final.
            // Track via a one-shot using buf[0].
            self.buf.clear();
            self.buf.push(1);
            // Note: we stay in StringTerm; the next byte branches via the
            // sentinel.
        } else if !self.buf.is_empty() && self.buf[0] == 1 {
            // Previous byte was ESC; this is the ST final. End the string
            // regardless of what `b` is — malformed sequences end here too.
            self.buf.clear();
            self.state = State::Ground;
        }
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
                // A DEC 2004 frame is emitted by the user's outer terminal
                // for its local clipboard action. An attached, authenticated
                // local terminal is the same user-intent boundary as `phux
                // paste`, which is trusted by default; classifying it as
                // untrusted makes the server reject normal multiline pastes
                // before delivering any bytes.
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

    /// Consume one of the three payload bytes of a legacy X10 mouse
    /// report. The three bytes are `Cb`, `Cx`, `Cy` — each is a raw
    /// single byte (NOT a numeric param), where the encoded value is
    /// `byte - 32` (clamped at zero for the rare under-32 byte). When
    /// the third byte arrives, decode and emit a [`MouseEvent`].
    fn feed_x10_mouse(&mut self, b: u8, bytes_seen: u8, out: &mut Vec<InputEvent>) {
        self.buf.push(b);
        let next = bytes_seen + 1;
        if next < 3 {
            self.state = State::X10Mouse { bytes_seen: next };
            return;
        }
        // We have all three bytes. Decode + emit, then return to ground.
        // Clone the three bytes out before clearing buf; the caller of
        // `feed_byte` holds a mutable borrow of `self.state`, but `buf`
        // is fine to move from.
        let cb = self.buf[0];
        let cx = self.buf[1];
        let cy = self.buf[2];
        self.buf.clear();
        self.state = State::Ground;
        dispatch_x10_mouse(cb, cx, cy, out);
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
            ascii_letter_to_key(letter).map(|key| KeyEvent {
                action: KeyAction::Press,
                key,
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
pub(super) const fn make_named_key(key: PhysicalKey, mods: ModSet) -> KeyEvent {
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

/// Map a printable ASCII byte to a [`PhysicalKey`].
///
/// Punctuation routes through [`phux_config::keybind::punct_to_key`] so
/// the runtime `KeyEvent` matches the same physical key the chord parser
/// emits for the same glyph in `default.toml` (phux-gxy). Without that
/// alignment, a chord like `"|"` → `{ key: Backslash, mods: SHIFT }` in
/// the resolver never matched the runtime `{ key: Unidentified, mods:
/// SHIFT }`, so every punctuation keybind silently forwarded to the
/// shell.
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
        b'A'..=b'Z' | b'a'..=b'z' => {
            // Lowercase first to look up the table.
            let upper = if b.is_ascii_lowercase() { b - 32 } else { b };
            ascii_letter_to_key_const(upper)
        }
        _ => match phux_config::keybind::punct_to_key(b as char) {
            Some((key, _shift)) => key,
            None => PhysicalKey::Unidentified,
        },
    }
}

/// Modifier set implied by a printable ASCII byte. Uppercase letters and
/// shifted punctuation (`!@#…`) come with `SHIFT` in `consumed_mods` per
/// SPEC §9.1.3 so the encoder doesn't double-apply.
const fn ascii_shift_mods(b: u8) -> ModSet {
    if b.is_ascii_uppercase() || is_shifted_punct(b) {
        ModSet::SHIFT
    } else {
        ModSet::empty()
    }
}

const fn is_shifted_punct(b: u8) -> bool {
    matches!(
        b,
        b'!' | b'@'
            | b'#'
            | b'$'
            | b'%'
            | b'^'
            | b'&'
            | b'*'
            | b'('
            | b')'
            | b'_'
            | b'+'
            | b'{'
            | b'}'
            | b'|'
            | b':'
            | b'"'
            | b'<'
            | b'>'
            | b'?'
            | b'~'
    )
}

/// What this key would produce with no modifiers held — used to fill
/// `unshifted_codepoint`. `A` → `a`, `@` → `2`, etc. Best-effort, US
/// QWERTY mapping; clients on other layouts can override at a higher
/// layer (not in scope here).
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
    // SGR mouse reports (DEC mode 1006) carry a leading `<` private-
    // marker byte and end in `M` (press / motion) or `m` (release).
    // Form: `CSI < <btn> ; <col> ; <row> M|m`. We dispatch them before
    // stripping the marker because the marker is what distinguishes
    // them from a bare `CSI M` (legacy X10 mouse, not supported).
    if matches!(params.first(), Some(&b'<')) && (final_byte == b'M' || final_byte == b'm') {
        dispatch_sgr_mouse(&params[1..], final_byte, out);
        return;
    }

    // urxvt-1015 decimal mouse: `CSI <btn> ; <col> ; <row> M`. Same
    // button bitfield as X10 (offset by 32) but transmitted as decimal
    // CSI parameters. Distinguished from SGR by the absence of the
    // leading `<` private-marker, and from `CSI 1;<mod>P..S` modifier-
    // bearing arrow / F-key forms (which have a letter final, not `M`).
    // Only triggers when there are no private-marker / intermediate
    // bytes — anything with a leading `?` / `=` / `>` is some other
    // private CSI we don't recognise.
    if final_byte == b'M'
        && !matches!(params.first(), Some(&b'?' | &b'<' | &b'=' | &b'>'))
        && params
            .iter()
            .all(|&b| matches!(b, b'0'..=b'9' | b';' | b':'))
    {
        let parsed = parse_csi_params(params);
        if parsed.len() == 3 {
            dispatch_urxvt1015_mouse(parsed[0], parsed[1], parsed[2], out);
            return;
        }
    }

    // Kitty keyboard protocol (KIP) `CSI u`. We target progressive-enhancement
    // levels 1 (disambiguate) + 2 (event types); sub-parameter groups for
    // higher levels (alternates, base-layout key, text codepoints) are parsed
    // out of the way and dropped.
    if final_byte == b'u' {
        dispatch_kitty_csi_u(params, out);
        return;
    }

    // Strip a leading private-marker `?`, `<`, `=`, `>` for now — we
    // don't differentiate, the modifier-bearing variant alone matters.
    let body = if let Some(first) = params.first()
        && matches!(*first, b'?' | b'<' | b'=' | b'>')
    {
        &params[1..]
    } else {
        params
    };

    // Parse out semicolon-separated numeric params. Missing/empty params
    // are treated as 0 (xterm convention; the final byte's interpretation
    // tells us what "missing" means in context).
    let parsed = parse_csi_params(body);

    // Focus reports (DEC mode 1004): `CSI I` = gained, `CSI O` = lost.
    // Recognised only when the parameter buffer is empty (bare CSI).
    // With parameters the same final bytes have other meanings.
    if body.is_empty() {
        if final_byte == b'I' {
            out.push(InputEvent::Focus(FocusEvent::Gained));
            return;
        }
        if final_byte == b'O' {
            out.push(InputEvent::Focus(FocusEvent::Lost));
            return;
        }
    }

    // The `CSI <n> ~` form encodes function keys and the navigation keys
    // (Insert, Delete, Home, End, PgUp/PgDn, F5..F12). The optional second
    // parameter is the xterm modifier code (1=none, 2=Shift, 3=Alt,
    // 5=Ctrl, etc.).
    if final_byte == b'~' {
        let n = parsed.first().copied().unwrap_or(1);
        let mods = parsed
            .get(1)
            .copied()
            .map_or(ModSet::empty(), xterm_modifier_code);
        if let Some(key) = csi_tilde_keycode(n) {
            out.push(InputEvent::Key(make_named_key(key, mods)));
        } else {
            tracing::trace!(n, final_byte, "unknown CSI ~ keycode");
        }
        return;
    }

    // The xterm modifier-bearing form is `CSI 1 ; <mod> <letter>` for the
    // arrow / Home / End / F1..F4 keys. When no modifier is present the
    // bare form `CSI <letter>` is used.
    let mods = if parsed.len() >= 2 && parsed[0] == 1 {
        xterm_modifier_code(parsed[1])
    } else {
        ModSet::empty()
    };

    if let Some(key) = csi_letter_keycode(final_byte) {
        out.push(InputEvent::Key(make_named_key(key, mods)));
        return;
    }

    tracing::trace!(final_byte, ?parsed, "unknown CSI sequence");
}

/// Decode an SGR-format mouse report's parameter region (the part after
/// the leading `<` and before the `M`/`m` final byte) and push a
/// [`MouseEvent`] into `out`. Silently drops malformed reports.
///
/// Format: `<btn> ; <col> ; <row>` where `<btn>` is a bitfield
/// (xterm SGR encoding, DEC mode 1006):
///
/// * bits 0-1: low button bits (0=L, 1=M, 2=R, 3=none/release-for-X10).
/// * bit 2:    Shift modifier.
/// * bit 3:    Alt (Meta) modifier.
/// * bit 4:    Ctrl modifier.
/// * bit 5:    motion (the report describes a drag / hover).
/// * bit 6:    wheel — buttons 4 (up) / 5 (down). High bit is the wheel
///   axis indicator in some terminals; we treat 64/65/66/67 as
///   the four wheel directions and pass them through as
///   libghostty `Button::Four` / `Five` / `Six` / `Seven`.
/// * bit 7:    extra buttons — 128..=131 → `Button::Eight..Eleven`.
///
/// `<col>` and `<row>` are 1-indexed cell coordinates. We convert to
/// 0-indexed `f64` pixels (treating "1 cell = 1 pixel" since the client
/// does not know cell-size here; per SPEC §9.2.1 the server's encoder
/// re-quantises for cell-format protocols).
fn dispatch_sgr_mouse(body: &[u8], final_byte: u8, out: &mut Vec<InputEvent>) {
    let parsed = parse_csi_params(body);
    if parsed.len() < 3 {
        tracing::trace!(?parsed, "malformed SGR mouse report (too few params)");
        return;
    }
    let raw_btn = parsed[0];
    let col = parsed[1];
    let row = parsed[2];

    let mods = sgr_mouse_mods(raw_btn);
    let button = sgr_mouse_button(raw_btn);
    let motion = (raw_btn & 0x20) != 0;
    let action = if motion {
        MouseAction::Motion
    } else if final_byte == b'm' {
        MouseAction::Release
    } else {
        MouseAction::Press
    };

    // 1-indexed → 0-indexed; coordinates are pane-local pixels per
    // SPEC §9.2.1 (integer-valued f64 from a cell-quantising client).
    #[allow(clippy::cast_lossless, reason = "u32 → f64 is exact for our range")]
    let x = (col.saturating_sub(1)) as f64;
    #[allow(clippy::cast_lossless, reason = "u32 → f64 is exact for our range")]
    let y = (row.saturating_sub(1)) as f64;

    out.push(InputEvent::Mouse(MouseEvent {
        action,
        button,
        mods,
        x,
        y,
    }));
}

/// Decode a kitty-protocol `CSI u` sequence.
///
/// Wire form (per <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>):
///
/// ```text
/// CSI keycode[:shifted_key:base_layout_key][;modifiers[:event_type[:text_codepoints]]] u
/// ```
///
/// `;` separates top-level parameter groups; `:` separates sub-parameters
/// within a group. We extract:
///
/// * group 0, sub 0 → keycode (Unicode codepoint of the unshifted key, or a
///   functional keycode in the PUA range — see `kitty_keycode_to_physical`).
/// * group 0, sub 1.. → `shifted_key`, `base_layout_key` (level 3,
///   `REPORT_ALTERNATES`). Parsed off the wire and **dropped** — no encoder
///   integration exists. Filed as a follow-up.
/// * group 1, sub 0 → modifier bitfield (`1 + shift|alt|ctrl|super|hyper|meta|caps_lock|num_lock`).
/// * group 1, sub 1 → event type (1=press, 2=repeat, 3=release; KIP level 2).
/// * group 1, sub 2.. → text codepoints (level 5, `REPORT_ASSOCIATED_TEXT`).
///   Decoded into [`KeyEvent::text`] whenever present, regardless of the
///   keycode mapping. Empty / zero sub-params are skipped.
///
/// Hyper / meta modifier bits are collapsed into [`ModSet::SUPER`] /
/// [`ModSet::ALT`] respectively — libghostty's `Mods` lacks distinct bits.
/// See [`kitty_modifier_code`].
fn dispatch_kitty_csi_u(params: &[u8], out: &mut Vec<InputEvent>) {
    let groups = parse_csi_param_groups(params);
    let keycode_group = groups.first();
    let keycode = keycode_group.and_then(|g| g.first().copied()).unwrap_or(0);
    if keycode == 0 {
        tracing::trace!("kitty CSI u with empty keycode, dropping");
        return;
    }

    // Level 3 (REPORT_ALTERNATES): shifted_key, base_layout_key sub-params.
    // Parsed for completeness; dropped pending encoder integration. Tracing
    // surfaces them so a future change can wire them through.
    if let Some(g) = keycode_group
        && g.len() > 1
    {
        tracing::trace!(
            keycode,
            shifted_key = ?g.get(1).copied(),
            base_layout_key = ?g.get(2).copied(),
            "kitty CSI u: dropping level-3 alternate sub-params",
        );
    }

    let mod_group = groups.get(1);
    let raw_mod = mod_group.and_then(|g| g.first().copied()).unwrap_or(1);
    let event_type = mod_group.and_then(|g| g.get(1).copied()).unwrap_or(1);

    let mods = kitty_modifier_code(raw_mod);
    let action = match event_type {
        3 => KeyAction::Release,
        2 => KeyAction::Repeat,
        _ => KeyAction::Press,
    };

    let Some(key) = kitty_keycode_to_physical(keycode) else {
        tracing::trace!(keycode, "kitty CSI u unmapped keycode");
        return;
    };

    // Level 5 (REPORT_ASSOCIATED_TEXT): text codepoints in group-1 sub-params
    // from index 2 onwards. Decode them into the `text` payload whenever
    // present. Empty / zero entries are skipped — they're "no associated text"
    // markers (KIP allows the terminal to emit modifier-only keys with an
    // empty text sub-param when the encoder has nothing to attribute).
    let text = mod_group.and_then(|g| {
        if g.len() <= 2 {
            return None;
        }
        let mut s = String::new();
        for &cp in &g[2..] {
            if cp == 0 {
                continue;
            }
            if let Some(c) = char::from_u32(cp) {
                s.push(c);
            } else {
                // Redaction-safe (ADR-0028): the codepoint is part of typed
                // text, so log only that one was malformed, never its value.
                tracing::trace!("kitty CSI u: invalid text codepoint, skipping");
            }
        }
        if s.is_empty() { None } else { Some(s) }
    });

    out.push(InputEvent::Key(KeyEvent {
        action,
        key,
        mods,
        consumed_mods: ModSet::empty(),
        composing: false,
        text,
        unshifted_codepoint: Some(keycode),
    }));
}

/// Parse CSI parameter bytes into nested groups: `;` opens a new top-level
/// group, `:` adds a sub-parameter to the current group. Empty slots become
/// `0`, matching xterm convention.
fn parse_csi_param_groups(body: &[u8]) -> Vec<Vec<u32>> {
    let mut groups: Vec<Vec<u32>> = Vec::new();
    let mut current: Vec<u32> = Vec::new();
    let mut acc: u32 = 0;
    let mut started = false;
    for &b in body {
        match b {
            b'0'..=b'9' => {
                acc = acc.saturating_mul(10).saturating_add(u32::from(b - b'0'));
                started = true;
            }
            b':' => {
                current.push(if started { acc } else { 0 });
                acc = 0;
                started = false;
            }
            b';' => {
                current.push(if started { acc } else { 0 });
                acc = 0;
                started = false;
                groups.push(std::mem::take(&mut current));
            }
            _ => {
                // Private-marker / intermediate / unrecognised — treat as
                // group separator for robustness.
                current.push(if started { acc } else { 0 });
                acc = 0;
                started = false;
                groups.push(std::mem::take(&mut current));
            }
        }
    }
    current.push(if started { acc } else { 0 });
    groups.push(current);
    groups
}

/// Kitty modifier bitfield → [`ModSet`].
///
/// KIP encodes modifiers as `1 + bitfield` (so an unmodified key reports `1`).
/// The bitfield is `shift=1, alt=2, ctrl=4, super=8, hyper=16, meta=32,
/// caps_lock=64, num_lock=128`. libghostty's [`ModSet`] does not expose
/// hyper / meta as distinct bits. For v0 we collapse them into their nearest
/// neighbours rather than drop:
///
/// * **hyper → `SUPER`.** Hyper is a less-common modifier that few
///   keyboards expose physically; binding it distinctly is rare and the
///   conventional X11 mapping treats Hyper and Super interchangeably.
/// * **meta → `ALT`.** KIP's "meta" mirrors the historical X11 Meta key,
///   which most modern keymaps fold into Alt/Option.
///
/// Caps lock and num lock pass through since libghostty's `Mods` carries them.
/// If a real use case for distinct hyper / meta bits materialises, we'll
/// either extend libghostty's `Mods` upstream or add a phux-side
/// `extra_mods` field; tracked as a follow-up.
fn kitty_modifier_code(code: u32) -> ModSet {
    if code == 0 {
        return ModSet::empty();
    }
    let bits = code.saturating_sub(1);
    let mut mods = ModSet::empty();
    if bits & 0b0000_0001 != 0 {
        mods |= ModSet::SHIFT;
    }
    if bits & 0b0000_0010 != 0 {
        mods |= ModSet::ALT;
    }
    if bits & 0b0000_0100 != 0 {
        mods |= ModSet::CTRL;
    }
    if bits & 0b0000_1000 != 0 {
        mods |= ModSet::SUPER;
    }
    // Hyper → SUPER (v0 collapse; libghostty's Mods has no distinct bit).
    if bits & 0b0001_0000 != 0 {
        mods |= ModSet::SUPER;
    }
    // Meta → ALT (v0 collapse; KIP "meta" mirrors the historical Meta key
    // which most modern keymaps fold into Alt/Option).
    if bits & 0b0010_0000 != 0 {
        mods |= ModSet::ALT;
    }
    if bits & 0b0100_0000 != 0 {
        mods |= ModSet::CAPS_LOCK;
    }
    if bits & 0b1000_0000 != 0 {
        mods |= ModSet::NUM_LOCK;
    }
    mods
}

/// Map a kitty keycode to a [`PhysicalKey`].
///
/// For printable Unicode codepoints (`U+0020..=U+007E` and beyond) we map the
/// ASCII range to its physical key, and otherwise emit `Unidentified` — the
/// codepoint travels in `unshifted_codepoint` so downstream consumers can
/// still recover the intent.
///
/// Functional keys use the kitty-defined PUA codepoints listed in the
/// protocol spec. Only the keys libghostty has a matching variant for are
/// mapped; everything else returns `None`.
#[allow(
    clippy::too_many_lines,
    reason = "flat keycode table — the size IS the spec"
)]
const fn kitty_keycode_to_physical(cp: u32) -> Option<PhysicalKey> {
    Some(match cp {
        // Legacy / ASCII region — these are explicitly defined by the
        // protocol for the keys that have a natural ASCII codepoint.
        27 => PhysicalKey::Escape,
        13 => PhysicalKey::Enter,
        9 => PhysicalKey::Tab,
        127 => PhysicalKey::Backspace,
        // Printable ASCII letters / digits / space.
        0x20 => PhysicalKey::Space,
        c @ 0x61..=0x7A => {
            // 'a'..='z'
            #[allow(clippy::cast_possible_truncation, reason = "range-checked u32 → u8")]
            let upper = (c as u8).to_ascii_uppercase();
            ascii_letter_to_key_const(upper)
        }
        c @ 0x41..=0x5A => {
            // 'A'..='Z'
            #[allow(clippy::cast_possible_truncation, reason = "range-checked u32 → u8")]
            let b = c as u8;
            ascii_letter_to_key_const(b)
        }
        c @ 0x30..=0x39 => {
            // '0'..='9'
            #[allow(clippy::cast_possible_truncation, reason = "range-checked u32 → u8")]
            let b = c as u8;
            ascii_to_physical(b)
        }
        // Functional keys (kitty PUA range). Codepoint constants are taken
        // verbatim from the kitty keyboard protocol "Functional key
        // definitions" table.
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
        // Other printable Unicode — no specific PhysicalKey, but the
        // codepoint survives in `unshifted_codepoint`.
        _ if cp >= 0x20 => PhysicalKey::Unidentified,
        _ => return None,
    })
}

/// Modifier bits in an SGR mouse button code.
fn sgr_mouse_mods(raw: u32) -> ModSet {
    let mut mods = ModSet::empty();
    if raw & 0x04 != 0 {
        mods |= ModSet::SHIFT;
    }
    if raw & 0x08 != 0 {
        mods |= ModSet::ALT;
    }
    if raw & 0x10 != 0 {
        mods |= ModSet::CTRL;
    }
    mods
}

/// Map the low / high button bits of an SGR mouse code to a
/// [`MouseButton`]. Returns `Button::Unknown` for the "no button"
/// motion case (`raw & 3 == 3`, motion bit set without a button).
const fn sgr_mouse_button(raw: u32) -> MouseButton {
    // Wheel reports: bit 6 set, low bits select axis/direction.
    if raw & 0x40 != 0 {
        return match raw & 0x03 {
            0 => MouseButton::Four,  // wheel up
            1 => MouseButton::Five,  // wheel down
            2 => MouseButton::Six,   // wheel left
            _ => MouseButton::Seven, // wheel right
        };
    }
    // Extra buttons: bit 7 set. xterm's "additional buttons" 8..=11.
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
        // Low 2 bits = 3 means "no button" (motion-without-button, or
        // explicit release in the legacy X10 form). SGR mode disambiguates
        // press vs. release via the `M`/`m` final byte; in either case
        // we report Unknown so the server-side encoder reconstructs the
        // correct PTY bytes from the action + position.
        _ => MouseButton::Unknown,
    }
}

/// Decode an X10 legacy mouse report (`CSI M Cb Cx Cy`) and push a
/// [`MouseEvent`].
///
/// X10 encodes:
///
/// * `Cb = (button | mods | motion) + 0x20` — so `Cb - 32` is the same
///   bitfield used by SGR mode (see [`sgr_mouse_button`] /
///   [`sgr_mouse_mods`]).
/// * `Cx = col + 0x20` — 1-indexed column, +32. `Cx = 0x21` is column 1.
/// * `Cy = row + 0x20` — 1-indexed row, +32.
///
/// Unlike SGR, X10 has no separate release final byte: a release is
/// reported with the low 2 button bits set to `3` (no button). We map
/// that to [`MouseAction::Release`] + [`MouseButton::Unknown`], matching
/// the legacy protocol's lack of per-button release tracking.
///
/// Coordinates are converted to 0-indexed `f64` pixels per SPEC §9.2.1
/// (the server's encoder re-quantises for cell-format protocols). A
/// `Cx` or `Cy` byte below `0x20` (illegal per the protocol but
/// observed in malformed streams) saturates at column / row 0.
fn dispatch_x10_mouse(cb: u8, cx: u8, cy: u8, out: &mut Vec<InputEvent>) {
    let raw_btn = u32::from(cb.saturating_sub(0x20));
    let col = u32::from(cx.saturating_sub(0x20));
    let row = u32::from(cy.saturating_sub(0x20));

    let mods = sgr_mouse_mods(raw_btn);
    let motion = (raw_btn & 0x20) != 0;
    // Bit 6 set = wheel report (treated as a press; releases for the
    // wheel aren't a thing in X10). Bit 7 set = extra buttons (also
    // press). Low 2 bits = 3 with no wheel / extra bit = release.
    let is_wheel_or_extra = (raw_btn & 0x40) != 0 || (raw_btn & 0x80) != 0;
    let action = if motion {
        MouseAction::Motion
    } else if !is_wheel_or_extra && (raw_btn & 0x03) == 0x03 {
        MouseAction::Release
    } else {
        MouseAction::Press
    };
    let button = sgr_mouse_button(raw_btn);

    // 1-indexed → 0-indexed; saturating to keep "col 1 → 0.0".
    #[allow(clippy::cast_lossless, reason = "u32 → f64 is exact for our range")]
    let x = (col.saturating_sub(1)) as f64;
    #[allow(clippy::cast_lossless, reason = "u32 → f64 is exact for our range")]
    let y = (row.saturating_sub(1)) as f64;

    out.push(InputEvent::Mouse(MouseEvent {
        action,
        button,
        mods,
        x,
        y,
    }));
}

/// Decode a urxvt-1015 decimal mouse report (`CSI <btn> ; <col> ; <row> M`)
/// and push a [`MouseEvent`].
///
/// urxvt-1015 uses the same button bitfield as X10 (offset by `0x20`)
/// but transmits all three values as decimal CSI parameters and always
/// terminates with `M`. Release is encoded the same way as X10: low 2
/// button bits = `3`, mapped to [`MouseAction::Release`] +
/// [`MouseButton::Unknown`].
///
/// We accept `btn` values in the X10 wire range (i.e. already offset by
/// 32). Values below 32 saturate at button code 0 — this matches the
/// behaviour of [`dispatch_x10_mouse`] for under-`0x20` `Cb` bytes.
fn dispatch_urxvt1015_mouse(btn: u32, col: u32, row: u32, out: &mut Vec<InputEvent>) {
    let raw_btn = btn.saturating_sub(0x20);

    let mods = sgr_mouse_mods(raw_btn);
    let motion = (raw_btn & 0x20) != 0;
    let is_wheel_or_extra = (raw_btn & 0x40) != 0 || (raw_btn & 0x80) != 0;
    let action = if motion {
        MouseAction::Motion
    } else if !is_wheel_or_extra && (raw_btn & 0x03) == 0x03 {
        MouseAction::Release
    } else {
        MouseAction::Press
    };
    let button = sgr_mouse_button(raw_btn);

    #[allow(clippy::cast_lossless, reason = "u32 → f64 is exact for our range")]
    let x = (col.saturating_sub(1)) as f64;
    #[allow(clippy::cast_lossless, reason = "u32 → f64 is exact for our range")]
    let y = (row.saturating_sub(1)) as f64;

    out.push(InputEvent::Mouse(MouseEvent {
        action,
        button,
        mods,
        x,
        y,
    }));
}

/// Parse semicolon-separated unsigned integers out of CSI parameter bytes.
/// Empty / non-digit slots become 0.
fn parse_csi_params(body: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut started = false;
    for &b in body {
        match b {
            b'0'..=b'9' => {
                acc = acc.saturating_mul(10).saturating_add(u32::from(b - b'0'));
                started = true;
            }
            b';' | b':' => {
                out.push(if started { acc } else { 0 });
                acc = 0;
                started = false;
            }
            _ => {
                // Intermediate / unrecognised byte; treat as separator.
                out.push(if started { acc } else { 0 });
                acc = 0;
                started = false;
            }
        }
    }
    out.push(if started { acc } else { 0 });
    out
}

/// xterm's `modifyCursorKeys` / `modifyFunctionKeys` modifier code →
/// [`ModSet`]. The code is (1 + sum of bit-weights) where Shift=1, Alt=2,
/// Ctrl=4, Super=8. Code 1 means no modifier.
fn xterm_modifier_code(code: u32) -> ModSet {
    if code == 0 {
        return ModSet::empty();
    }
    let bits = code.saturating_sub(1);
    let mut mods = ModSet::empty();
    if bits & 0b0001 != 0 {
        mods |= ModSet::SHIFT;
    }
    if bits & 0b0010 != 0 {
        mods |= ModSet::ALT;
    }
    if bits & 0b0100 != 0 {
        mods |= ModSet::CTRL;
    }
    if bits & 0b1000 != 0 {
        mods |= ModSet::SUPER;
    }
    mods
}

/// `CSI <letter>` keycodes. Returns `None` for letters we don't recognise
/// (e.g. `CSI M` mouse reports, which we skip pending follow-up).
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

/// Map ASCII uppercase letter bytes to libghostty's `PhysicalKey` variants.
const fn ascii_letter_to_key(b: u8) -> Option<PhysicalKey> {
    if !b.is_ascii_uppercase() {
        return None;
    }
    Some(ascii_letter_to_key_const(b))
}

const fn ascii_letter_to_key_const(b: u8) -> PhysicalKey {
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
    use phux_protocol::ids::TerminalId;
    use phux_protocol::wire::frame::FrameKind;

    use super::*;

    fn key_only(evs: &[InputEvent]) -> Vec<&KeyEvent> {
        evs.iter()
            .filter_map(|e| {
                if let InputEvent::Key(k) = e {
                    Some(k)
                } else {
                    None
                }
            })
            .collect()
    }

    // ---- Plain bytes -----------------------------------------------------

    #[test]
    fn printable_byte_becomes_key_event_with_text() {
        let mut p = StdinParser::new();
        let evs = p.feed(b"a");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].text.as_deref(), Some("a"));
        assert_eq!(keys[0].action, KeyAction::Press);
        assert_eq!(keys[0].key, PhysicalKey::A);
    }

    #[test]
    fn uppercase_carries_shift_mod() {
        let mut p = StdinParser::new();
        let evs = p.feed(b"A");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert!(keys[0].mods.contains(ModSet::SHIFT));
        assert!(keys[0].consumed_mods.contains(ModSet::SHIFT));
        assert_eq!(keys[0].unshifted_codepoint, Some(u32::from('a')));
    }

    // phux-gxy regression: punctuation glyphs must map to the same
    // physical key the chord parser emits, or `default.toml` chords
    // like `"|"` (split-pane) never match.
    #[test]
    fn pipe_maps_to_backslash_with_shift() {
        let mut p = StdinParser::new();
        let evs = p.feed(b"|");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::Backslash);
        assert!(keys[0].mods.contains(ModSet::SHIFT));
    }

    #[test]
    fn minus_maps_to_minus_unshifted() {
        let mut p = StdinParser::new();
        let evs = p.feed(b"-");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::Minus);
        assert!(!keys[0].mods.contains(ModSet::SHIFT));
    }

    #[test]
    fn punctuation_matches_chord_parser_for_default_split_binds() {
        // The default config uses `"|"` and `"-"` as split chords; the
        // resolver builds these via parse_chord. The runtime parser
        // must emit the same (key, mods) shape or the chord never fires.
        for &glyph in b"|-`=[]\\;',./~!@#$%^&*()_+{}:\"<>?" {
            let mut p = StdinParser::new();
            let evs = p.feed(&[glyph]);
            let keys = key_only(&evs);
            assert_eq!(keys.len(), 1, "no key for glyph {}", char::from(glyph));
            let parser_key = (keys[0].key, keys[0].mods.contains(ModSet::SHIFT));
            let chord_key = phux_config::keybind::parse_chord(&char::from(glyph).to_string())
                .expect("chord parse")
                .modifiers
                .contains(ModSet::SHIFT);
            let chord_pk = phux_config::keybind::parse_chord(&char::from(glyph).to_string())
                .expect("chord parse")
                .key;
            assert_eq!(
                parser_key,
                (chord_pk, chord_key),
                "mismatch on glyph {}",
                char::from(glyph)
            );
        }
    }

    #[test]
    fn enter_byte_becomes_enter_key() {
        let mut p = StdinParser::new();
        let evs = p.feed(b"\r");
        let keys = key_only(&evs);
        assert_eq!(keys[0].key, PhysicalKey::Enter);
    }

    #[test]
    fn ctrl_c_byte_becomes_ctrl_modified_c() {
        let mut p = StdinParser::new();
        let evs = p.feed(&[0x03]);
        let keys = key_only(&evs);
        assert_eq!(keys[0].key, PhysicalKey::C);
        assert!(keys[0].mods.contains(ModSet::CTRL));
    }

    #[test]
    fn lf_byte_becomes_ctrl_j_not_enter() {
        // 0x0A (Ctrl+J) must not be laundered into Enter; the server encoder
        // re-derives bytes from the KeyEvent, so an Enter event would emit CR
        // and swallow the line feed.
        let mut p = StdinParser::new();
        let evs = p.feed(&[0x0A]);
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::J);
        assert_eq!(keys[0].mods, ModSet::CTRL);
    }

    // ---- UTF-8 ----------------------------------------------------------

    #[test]
    fn two_byte_utf8_becomes_one_key_event() {
        let mut p = StdinParser::new();
        // U+00E9 LATIN SMALL LETTER E WITH ACUTE → 0xC3 0xA9
        let evs = p.feed(&[0xC3, 0xA9]);
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].text.as_deref(), Some("é"));
        assert_eq!(keys[0].unshifted_codepoint, Some(0x00E9));
    }

    #[test]
    fn three_byte_utf8_across_two_feeds() {
        let mut p = StdinParser::new();
        // U+1F600 GRINNING FACE → 0xF0 0x9F 0x98 0x80
        let first = p.feed(&[0xF0, 0x9F]);
        assert!(first.is_empty());
        let second = p.feed(&[0x98, 0x80]);
        let keys = key_only(&second);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].text.as_deref(), Some("😀"));
    }

    #[test]
    fn invalid_utf8_continuation_recovers() {
        let mut p = StdinParser::new();
        // Lead 0xC3 expects one continuation; give it 'a' instead.
        let evs = p.feed(&[0xC3, b'a']);
        // We expect `a` to come through after the bad sequence is dropped.
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].text.as_deref(), Some("a"));
    }

    // ---- Configurable detach key input ---------------------------------

    #[test]
    fn ctrl_b_is_regular_key_event_for_keybinding_resolver() {
        let mut p = StdinParser::new();
        let evs = p.feed(&[0x02, b'd']);
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].key, PhysicalKey::B);
        assert_eq!(keys[0].mods, ModSet::CTRL);
        assert_eq!(keys[1].text.as_deref(), Some("d"));
    }

    // ---- Bare ESC / Alt-chord (timing) ----------------------------------

    #[test]
    fn esc_byte_alone_does_not_emit_immediately() {
        let mut p = StdinParser::new();
        let evs = p.feed(&[0x1B]);
        assert!(evs.is_empty(), "bare ESC must wait for flush");
        assert!(p.has_pending());
    }

    #[test]
    fn esc_byte_then_flush_emits_escape_key() {
        let mut p = StdinParser::new();
        let _ = p.feed(&[0x1B]);
        let flushed = p.flush();
        let keys = key_only(&flushed);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::Escape);
        assert!(!p.has_pending());
    }

    #[test]
    fn esc_then_char_in_same_feed_is_alt_chord() {
        let mut p = StdinParser::new();
        let evs = p.feed(&[0x1B, b'a']);
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::A);
        assert!(keys[0].mods.contains(ModSet::ALT));
    }

    #[test]
    fn esc_then_char_across_two_feeds_is_alt_chord() {
        let mut p = StdinParser::new();
        let first = p.feed(&[0x1B]);
        assert!(first.is_empty());
        let second = p.feed(b"x");
        let keys = key_only(&second);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::X);
        assert!(keys[0].mods.contains(ModSet::ALT));
    }

    #[test]
    fn double_esc_emits_escape_then_pending() {
        let mut p = StdinParser::new();
        let evs = p.feed(&[0x1B, 0x1B]);
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::Escape);
        assert!(p.has_pending());
    }

    // ---- CSI arrow keys -------------------------------------------------

    #[test]
    fn csi_arrow_up_unmod() {
        let mut p = StdinParser::new();
        let evs = p.feed(b"\x1b[A");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::ArrowUp);
        assert!(keys[0].mods.is_empty());
    }

    #[test]
    fn csi_ctrl_arrow_up() {
        let mut p = StdinParser::new();
        let evs = p.feed(b"\x1b[1;5A");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::ArrowUp);
        assert!(keys[0].mods.contains(ModSet::CTRL));
        assert!(!keys[0].mods.contains(ModSet::SHIFT));
    }

    #[test]
    fn csi_shift_alt_ctrl_arrow_right() {
        let mut p = StdinParser::new();
        // mod = 1 + (Shift=1 + Alt=2 + Ctrl=4) = 8
        let evs = p.feed(b"\x1b[1;8C");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::ArrowRight);
        assert!(keys[0].mods.contains(ModSet::SHIFT));
        assert!(keys[0].mods.contains(ModSet::ALT));
        assert!(keys[0].mods.contains(ModSet::CTRL));
    }

    // ---- CSI tilde-form keys --------------------------------------------

    #[test]
    fn csi_home_end_via_letter_and_tilde() {
        let mut p = StdinParser::new();
        let letter = p.feed(b"\x1b[H");
        assert_eq!(key_only(&letter)[0].key, PhysicalKey::Home);
        let tilde = p.feed(b"\x1b[1~");
        assert_eq!(key_only(&tilde)[0].key, PhysicalKey::Home);
    }

    #[test]
    fn csi_pageup_unmod_and_ctrl() {
        let mut p = StdinParser::new();
        let unmod = p.feed(b"\x1b[5~");
        assert_eq!(key_only(&unmod)[0].key, PhysicalKey::PageUp);
        let ctrl = p.feed(b"\x1b[5;5~");
        let keys = key_only(&ctrl);
        assert_eq!(keys[0].key, PhysicalKey::PageUp);
        assert!(keys[0].mods.contains(ModSet::CTRL));
    }

    #[test]
    fn csi_f5_through_f12() {
        let cases = [
            (b"\x1b[15~".as_slice(), PhysicalKey::F5),
            (b"\x1b[17~", PhysicalKey::F6),
            (b"\x1b[18~", PhysicalKey::F7),
            (b"\x1b[19~", PhysicalKey::F8),
            (b"\x1b[20~", PhysicalKey::F9),
            (b"\x1b[21~", PhysicalKey::F10),
            (b"\x1b[23~", PhysicalKey::F11),
            (b"\x1b[24~", PhysicalKey::F12),
        ];
        for (input, expected) in cases {
            let mut p = StdinParser::new();
            let evs = p.feed(input);
            let keys = key_only(&evs);
            assert_eq!(keys.len(), 1, "input {input:?} produced {evs:?}");
            assert_eq!(keys[0].key, expected, "input {input:?}");
        }
    }

    // ---- SS3 ------------------------------------------------------------

    #[test]
    fn ss3_f1_through_f4() {
        let cases = [
            (b"\x1bOP".as_slice(), PhysicalKey::F1),
            (b"\x1bOQ", PhysicalKey::F2),
            (b"\x1bOR", PhysicalKey::F3),
            (b"\x1bOS", PhysicalKey::F4),
        ];
        for (input, expected) in cases {
            let mut p = StdinParser::new();
            let evs = p.feed(input);
            let keys = key_only(&evs);
            assert_eq!(keys.len(), 1, "input {input:?}");
            assert_eq!(keys[0].key, expected, "input {input:?}");
        }
    }

    #[test]
    fn ss3_arrow_keys_in_application_mode() {
        let mut p = StdinParser::new();
        let evs = p.feed(b"\x1bOA");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::ArrowUp);
    }

    // ---- Partial CSI across reads ---------------------------------------

    #[test]
    fn csi_split_across_three_feeds() {
        let mut p = StdinParser::new();
        let a = p.feed(b"\x1b");
        assert!(a.is_empty());
        let b = p.feed(b"[1;");
        assert!(b.is_empty());
        let c = p.feed(b"5A");
        let keys = key_only(&c);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::ArrowUp);
        assert!(keys[0].mods.contains(ModSet::CTRL));
    }

    #[test]
    fn partial_csi_keeps_pending_flag() {
        let mut p = StdinParser::new();
        let _ = p.feed(b"\x1b[1;");
        assert!(p.has_pending());
        // flush() must NOT consume a half-finished CSI.
        let flushed = p.flush();
        assert!(flushed.is_empty());
        assert!(p.has_pending());
        let final_evs = p.feed(b"5A");
        assert_eq!(key_only(&final_evs).len(), 1);
    }

    /// phux-l96p.4. The idle timer exists to disambiguate a lone ESC, and
    /// `flush` only ever emits from that one state — so `esc_pending` must be
    /// true for a bare ESC and false for every other in-progress sequence,
    /// even the ones `has_pending` still reports. Arming on the broader
    /// predicate cost a guaranteed-useless 10ms wake-up whenever a multi-byte
    /// key landed split across a read boundary.
    #[test]
    fn only_a_lone_esc_arms_the_idle_flush() {
        let mut p = StdinParser::new();
        assert!(!p.esc_pending(), "ground state waits for nothing");

        let _ = p.feed(b"\x1b");
        assert!(
            p.esc_pending(),
            "a bare ESC is exactly what the timer is for"
        );

        let _ = p.feed(b"[1;");
        assert!(p.has_pending(), "the CSI is still in progress");
        assert!(
            !p.esc_pending(),
            "a partial CSI cannot flush, so it must not arm the timer",
        );

        let evs = p.feed(b"5A");
        assert_eq!(key_only(&evs).len(), 1, "the CSI completes on its own");
        assert!(!p.esc_pending());
    }

    /// The attach loop feeds into one retained buffer. `feed_into` must append
    /// to what is already there rather than replace it, or a batch would lose
    /// every event decoded before the last read.
    #[test]
    fn feed_into_appends_to_the_callers_buffer() {
        let mut p = StdinParser::new();
        let mut out = Vec::new();
        p.feed_into(b"ab", &mut out);
        p.feed_into(b"c", &mut out);
        assert_eq!(key_only(&out).len(), 3);

        // ... and `flush_into` joins the same buffer.
        p.feed_into(b"\x1b", &mut out);
        assert!(p.esc_pending());
        p.flush_into(&mut out);
        let keys = key_only(&out);
        assert_eq!(keys.len(), 4);
        assert_eq!(keys[3].key, PhysicalKey::Escape);
        assert!(!p.has_pending());
    }

    // ---- Misc -----------------------------------------------------------

    #[test]
    fn over_long_csi_aborts_cleanly() {
        let mut p = StdinParser::new();
        let mut s = Vec::from(b"\x1b[".as_slice());
        s.extend(std::iter::repeat_n(b'1', 300));
        s.push(b'A');
        let _ = p.feed(&s);
        assert!(!p.has_pending(), "parser must recover after overflow");
    }

    #[test]
    fn unknown_csi_final_is_dropped_silently() {
        let mut p = StdinParser::new();
        // Pick a CSI sequence with a final byte we genuinely don't
        // recognise (lowercase `z`) — the original `CSI M ...` form
        // is now consumed by the X10 mouse parser. The contract here
        // is just that the parser doesn't panic and returns to the
        // ground state cleanly.
        let _ = p.feed(b"\x1b[1;2z");
        assert!(!p.has_pending());
    }

    #[test]
    fn alt_letter_strips_text() {
        let mut p = StdinParser::new();
        let evs = p.feed(b"\x1ba");
        let keys = key_only(&evs);
        assert_eq!(keys[0].text, None);
        assert!(keys[0].mods.contains(ModSet::ALT));
    }

    #[test]
    fn into_frame_carries_terminal_id() {
        let key = KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::A,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: Some("a".to_owned()),
            unshifted_codepoint: Some(u32::from('a')),
        };
        let frame = InputEvent::Key(key).into_frame(TerminalId::local(42));
        match frame {
            FrameKind::InputKey { terminal_id, .. } => {
                assert_eq!(terminal_id, TerminalId::local(42));
            }
            other => panic!("expected InputKey, got {other:?}"),
        }
    }

    // ---- Focus reports (DEC mode 1004) ---------------------------------

    fn focus_only(evs: &[InputEvent]) -> Vec<FocusEvent> {
        evs.iter()
            .filter_map(|e| {
                if let InputEvent::Focus(f) = e {
                    Some(*f)
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn csi_capital_i_is_focus_gained() {
        let mut p = StdinParser::new();
        let evs = p.feed(b"\x1b[I");
        let f = focus_only(&evs);
        assert_eq!(f, vec![FocusEvent::Gained]);
    }

    #[test]
    fn csi_capital_o_is_focus_lost() {
        let mut p = StdinParser::new();
        let evs = p.feed(b"\x1b[O");
        let f = focus_only(&evs);
        assert_eq!(f, vec![FocusEvent::Lost]);
    }

    #[test]
    fn focus_event_into_frame_carries_terminal_id() {
        let frame = InputEvent::Focus(FocusEvent::Gained).into_frame(TerminalId::new(7));
        match frame {
            FrameKind::InputFocus { terminal_id, event } => {
                assert_eq!(terminal_id, TerminalId::new(7));
                assert_eq!(event, FocusEvent::Gained);
            }
            other => panic!("expected InputFocus, got {other:?}"),
        }
    }

    #[test]
    fn ss3_capital_o_still_routes_via_ss3_not_focus() {
        // ESC O P is SS3 F1, not focus. Ensures the new focus dispatch
        // didn't accidentally consume the SS3 path.
        let mut p = StdinParser::new();
        let evs = p.feed(b"\x1bOP");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::F1);
        assert!(focus_only(&evs).is_empty());
    }

    // ---- SGR mouse reports (DEC mode 1006) ------------------------------

    fn mouse_only(evs: &[InputEvent]) -> Vec<MouseEvent> {
        evs.iter()
            .filter_map(|e| {
                if let InputEvent::Mouse(m) = e {
                    Some(*m)
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn sgr_left_press_and_release() {
        let mut p = StdinParser::new();
        let press = p.feed(b"\x1b[<0;5;3M");
        let m = mouse_only(&press);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].action, MouseAction::Press);
        assert_eq!(m[0].button, MouseButton::Left);
        assert!(m[0].mods.is_empty());
        // 1-indexed → 0-indexed: col 5 → 4.0, row 3 → 2.0.
        assert!((m[0].x - 4.0).abs() < f64::EPSILON);
        assert!((m[0].y - 2.0).abs() < f64::EPSILON);

        let release = p.feed(b"\x1b[<0;5;3m");
        let m2 = mouse_only(&release);
        assert_eq!(m2.len(), 1);
        assert_eq!(m2[0].action, MouseAction::Release);
        assert_eq!(m2[0].button, MouseButton::Left);
    }

    #[test]
    fn sgr_right_middle_buttons() {
        let mut p = StdinParser::new();
        let right = p.feed(b"\x1b[<2;1;1M");
        assert_eq!(mouse_only(&right)[0].button, MouseButton::Right);
        let middle = p.feed(b"\x1b[<1;1;1M");
        assert_eq!(mouse_only(&middle)[0].button, MouseButton::Middle);
    }

    #[test]
    fn sgr_motion_with_button() {
        let mut p = StdinParser::new();
        // bit 5 (0x20) is motion. 0x20 | 0 = 32 → Left button drag.
        let evs = p.feed(b"\x1b[<32;10;5M");
        let m = mouse_only(&evs);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].action, MouseAction::Motion);
        assert_eq!(m[0].button, MouseButton::Left);
    }

    #[test]
    fn sgr_motion_no_button() {
        let mut p = StdinParser::new();
        // 0x20 (motion) | 0x03 (no-button) = 35.
        let evs = p.feed(b"\x1b[<35;1;1M");
        let m = mouse_only(&evs);
        assert_eq!(m[0].action, MouseAction::Motion);
        assert_eq!(m[0].button, MouseButton::Unknown);
    }

    #[test]
    fn sgr_wheel_up_down() {
        let mut p = StdinParser::new();
        // 64 = wheel up, 65 = wheel down.
        let up = p.feed(b"\x1b[<64;1;1M");
        assert_eq!(mouse_only(&up)[0].button, MouseButton::Four);
        let down = p.feed(b"\x1b[<65;1;1M");
        assert_eq!(mouse_only(&down)[0].button, MouseButton::Five);
    }

    #[test]
    fn sgr_with_modifiers() {
        let mut p = StdinParser::new();
        // 0 (Left) | 4 (Shift) | 8 (Alt) | 16 (Ctrl) = 28.
        let evs = p.feed(b"\x1b[<28;1;1M");
        let m = mouse_only(&evs);
        assert_eq!(m[0].button, MouseButton::Left);
        assert!(m[0].mods.contains(ModSet::SHIFT));
        assert!(m[0].mods.contains(ModSet::ALT));
        assert!(m[0].mods.contains(ModSet::CTRL));
    }

    #[test]
    fn sgr_mouse_split_across_feeds() {
        let mut parser = StdinParser::new();
        let first = parser.feed(b"\x1b[<0");
        assert!(mouse_only(&first).is_empty());
        let middle = parser.feed(b";5;3");
        assert!(mouse_only(&middle).is_empty());
        let last = parser.feed(b"M");
        let evs = mouse_only(&last);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].action, MouseAction::Press);
    }

    #[test]
    fn sgr_mouse_into_frame_carries_terminal_id() {
        let ev = MouseEvent {
            action: MouseAction::Press,
            button: MouseButton::Left,
            mods: ModSet::empty(),
            x: 1.0,
            y: 2.0,
        };
        let frame = InputEvent::Mouse(ev).into_frame(TerminalId::new(99));
        match frame {
            FrameKind::InputMouse { terminal_id, .. } => {
                assert_eq!(terminal_id, TerminalId::new(99));
            }
            other => panic!("expected InputMouse, got {other:?}"),
        }
    }

    // ---- Legacy X10 mouse (CSI M Cb Cx Cy) -----------------------------

    #[test]
    fn x10_left_press() {
        let mut p = StdinParser::new();
        // Cb = 0 + 32 = 0x20 (Left press, no mods, no motion).
        // Cx = 1 + 32 = 0x21 (column 1).
        // Cy = 1 + 32 = 0x21 (row 1).
        let evs = p.feed(b"\x1b[M\x20\x21\x21");
        let m = mouse_only(&evs);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].action, MouseAction::Press);
        assert_eq!(m[0].button, MouseButton::Left);
        assert!(m[0].mods.is_empty());
        assert!((m[0].x - 0.0).abs() < f64::EPSILON);
        assert!((m[0].y - 0.0).abs() < f64::EPSILON);
        assert!(!p.has_pending());
    }

    #[test]
    fn x10_release_maps_to_release_action() {
        let mut p = StdinParser::new();
        // Cb = 3 + 32 = 0x23 (no-button = release in X10).
        // Cx, Cy = col 5, row 3 → bytes 0x25, 0x23.
        let evs = p.feed(b"\x1b[M\x23\x25\x23");
        let m = mouse_only(&evs);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].action, MouseAction::Release);
        // X10 release does not identify which button was released.
        assert_eq!(m[0].button, MouseButton::Unknown);
        // col 5 → 4.0, row 3 → 2.0.
        assert!((m[0].x - 4.0).abs() < f64::EPSILON);
        assert!((m[0].y - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn x10_left_press_with_shift_modifier() {
        let mut p = StdinParser::new();
        // Cb = (0 | 4) + 32 = 0x24 (Left press, Shift held).
        let evs = p.feed(b"\x1b[M\x24\x21\x21");
        let m = mouse_only(&evs);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].action, MouseAction::Press);
        assert_eq!(m[0].button, MouseButton::Left);
        assert!(m[0].mods.contains(ModSet::SHIFT));
        assert!(!m[0].mods.contains(ModSet::ALT));
        assert!(!m[0].mods.contains(ModSet::CTRL));
    }

    #[test]
    fn x10_payload_bytes_with_high_bit_do_not_break_parser() {
        // X10 payload bytes can be anything in `0x20..` — in particular
        // they can exceed printable ASCII for large terminals. The
        // dedicated consumer state must not interpret them as new
        // sequences (e.g. as a stray ESC). Use a row byte of 0xFF and
        // verify the parser returns cleanly to ground.
        let mut p = StdinParser::new();
        let evs = p.feed(b"\x1b[M\x20\x21\xff");
        let m = mouse_only(&evs);
        assert_eq!(m.len(), 1);
        assert!(!p.has_pending());
    }

    #[test]
    fn x10_mouse_split_across_feeds() {
        let mut parser = StdinParser::new();
        // Feed the CSI M intro and the three payload bytes one at a time.
        assert!(mouse_only(&parser.feed(b"\x1b[")).is_empty());
        assert!(mouse_only(&parser.feed(b"M")).is_empty());
        assert!(parser.has_pending());
        assert!(mouse_only(&parser.feed(b"\x20")).is_empty());
        assert!(mouse_only(&parser.feed(b"\x21")).is_empty());
        let last = parser.feed(b"\x21");
        let m = mouse_only(&last);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].action, MouseAction::Press);
        assert_eq!(m[0].button, MouseButton::Left);
        assert!(!parser.has_pending());
    }

    // ---- urxvt-1015 decimal mouse (CSI <btn>;<col>;<row> M) ------------

    #[test]
    fn urxvt1015_left_press() {
        let mut p = StdinParser::new();
        // urxvt-1015 button is the X10 byte value: Left press = 0 + 32 = 32.
        let evs = p.feed(b"\x1b[32;5;3M");
        let m = mouse_only(&evs);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].action, MouseAction::Press);
        assert_eq!(m[0].button, MouseButton::Left);
        assert!(m[0].mods.is_empty());
        assert!((m[0].x - 4.0).abs() < f64::EPSILON);
        assert!((m[0].y - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn urxvt1015_release() {
        let mut p = StdinParser::new();
        // Release: low 2 bits = 3, so btn = 3 + 32 = 35.
        let evs = p.feed(b"\x1b[35;5;3M");
        let m = mouse_only(&evs);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].action, MouseAction::Release);
        assert_eq!(m[0].button, MouseButton::Unknown);
    }

    #[test]
    fn urxvt1015_wheel_up_is_a_press() {
        let mut p = StdinParser::new();
        // Wheel up: bit 6 set, low bits 0. raw = 64, wire = 64 + 32 = 96.
        let evs = p.feed(b"\x1b[96;1;1M");
        let m = mouse_only(&evs);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].action, MouseAction::Press);
        assert_eq!(m[0].button, MouseButton::Four);
    }

    #[test]
    fn urxvt1015_does_not_collide_with_sgr() {
        // SGR carries a leading `<`. The urxvt-1015 branch must NOT
        // dispatch when the marker is present.
        let mut p = StdinParser::new();
        let evs = p.feed(b"\x1b[<0;5;3M");
        let m = mouse_only(&evs);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].action, MouseAction::Press);
        assert_eq!(m[0].button, MouseButton::Left);
        // The body of this test is identical to sgr_left_press_and_release;
        // its purpose is to assert the dispatch order — adding the
        // urxvt-1015 branch must not steal SGR reports.
    }

    // ---- Bracketed paste (DEC mode 2004) -------------------------------

    fn paste_only(evs: &[InputEvent]) -> Vec<PasteEvent> {
        evs.iter()
            .filter_map(|e| {
                if let InputEvent::Paste(p) = e {
                    Some(p.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn bracketed_paste_basic_round_trip() {
        let mut p = StdinParser::new();
        let evs = p.feed(b"\x1b[200~hello world\x1b[201~");
        let pastes = paste_only(&evs);
        assert_eq!(pastes.len(), 1);
        assert_eq!(pastes[0].data, b"hello world");
        assert_eq!(pastes[0].trust, PasteTrust::Trusted);
        // No key events leaked from inside the brackets.
        assert!(key_only(&evs).is_empty());
        assert!(!p.has_pending());
    }

    #[test]
    fn bracketed_paste_split_across_feeds() {
        let mut p = StdinParser::new();
        let a = p.feed(b"\x1b[200~");
        assert!(a.is_empty());
        assert!(p.has_pending());
        let b = p.feed(b"abc");
        assert!(b.is_empty());
        let c = p.feed(b"\x1b[201~");
        let pastes = paste_only(&c);
        assert_eq!(pastes.len(), 1);
        assert_eq!(pastes[0].data, b"abc");
    }

    #[test]
    fn bracketed_paste_payload_with_inner_csi() {
        let mut p = StdinParser::new();
        // User pastes a string that contains an ANSI color escape — the
        // ESC + CSI inside the payload must be preserved, not consumed
        // as a close marker.
        let payload = b"red\x1b[31mthing\x1b[0mend";
        let mut s = Vec::from(b"\x1b[200~".as_slice());
        s.extend_from_slice(payload);
        s.extend_from_slice(b"\x1b[201~");
        let evs = p.feed(&s);
        let pastes = paste_only(&evs);
        assert_eq!(pastes.len(), 1);
        assert_eq!(pastes[0].data, payload);
        assert!(!p.has_pending());
    }

    #[test]
    fn bracketed_paste_with_bare_esc_in_payload() {
        // A bare ESC inside the paste should remain in the payload.
        let mut p = StdinParser::new();
        let mut s = Vec::from(b"\x1b[200~".as_slice());
        s.extend_from_slice(b"a\x1bb");
        s.extend_from_slice(b"\x1b[201~");
        let evs = p.feed(&s);
        let pastes = paste_only(&evs);
        assert_eq!(pastes.len(), 1);
        assert_eq!(pastes[0].data, b"a\x1bb");
    }

    #[test]
    fn bracketed_paste_into_frame_carries_terminal_id() {
        let frame = InputEvent::Paste(PasteEvent {
            trust: PasteTrust::Trusted,
            data: b"x".to_vec(),
        })
        .into_frame(TerminalId::new(11));
        match frame {
            FrameKind::InputPaste { terminal_id, event } => {
                assert_eq!(terminal_id, TerminalId::new(11));
                assert_eq!(event.data, b"x");
            }
            other => panic!("expected InputPaste, got {other:?}"),
        }
    }

    // ---- Kitty CSI u (KIP level 1 + 2) ---------------------------------

    #[test]
    fn kitty_csi_u_plain_lowercase_letter() {
        let mut p = StdinParser::new();
        let evs = p.feed(b"\x1b[97u");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::A);
        assert_eq!(keys[0].action, KeyAction::Press);
        assert!(keys[0].mods.is_empty());
        assert_eq!(keys[0].unshifted_codepoint, Some(97));
    }

    #[test]
    fn kitty_csi_u_ctrl_a() {
        let mut p = StdinParser::new();
        // modifiers = 1 + ctrl(4) = 5
        let evs = p.feed(b"\x1b[97;5u");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::A);
        assert_eq!(keys[0].mods, ModSet::CTRL);
        assert_eq!(keys[0].action, KeyAction::Press);
    }

    #[test]
    fn kitty_csi_u_f_key_via_pua_codepoint() {
        let mut p = StdinParser::new();
        // F5 = 57368
        let evs = p.feed(b"\x1b[57368u");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::F5);
        assert!(keys[0].mods.is_empty());
    }

    #[test]
    fn kitty_csi_u_release_event_type() {
        let mut p = StdinParser::new();
        // CSI 97;1:3 u — press-modifier baseline (1 = no mods), release.
        let evs = p.feed(b"\x1b[97;1:3u");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::A);
        assert_eq!(keys[0].action, KeyAction::Release);
    }

    #[test]
    fn kitty_csi_u_multiple_modifiers() {
        let mut p = StdinParser::new();
        // mods = 1 + shift(1) + alt(2) + ctrl(4) = 8
        let evs = p.feed(b"\x1b[97;8u");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::A);
        assert!(keys[0].mods.contains(ModSet::SHIFT));
        assert!(keys[0].mods.contains(ModSet::ALT));
        assert!(keys[0].mods.contains(ModSet::CTRL));
    }

    #[test]
    fn kitty_csi_u_repeat_event_maps_to_repeat_action() {
        let mut p = StdinParser::new();
        // event_type = 2 = repeat.
        let evs = p.feed(b"\x1b[97;1:2u");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1, "repeat must emit a key event, not drop");
        assert_eq!(keys[0].key, PhysicalKey::A);
        assert_eq!(keys[0].action, KeyAction::Repeat);
    }

    #[test]
    fn kitty_csi_u_alternate_keys_subparams_ignored() {
        let mut p = StdinParser::new();
        // Level 3: keycode with shifted_key and base_layout_key sub-params.
        // CSI 97:65:97 ; 2 u — 'a' shifted to 'A' (US layout). We absorb the
        // shifted/base sub-params and treat the event as a plain Shift-A.
        let evs = p.feed(b"\x1b[97:65:97;2u");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::A);
        assert!(keys[0].mods.contains(ModSet::SHIFT));
    }

    #[test]
    fn kitty_csi_u_text_codepoints_populate_text() {
        let mut p = StdinParser::new();
        // Level 5: CSI 97 ; 1 : 1 : 97 u — 'a' press with explicit text 'a'.
        let evs = p.feed(b"\x1b[97;1:1:97u");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::A);
        assert_eq!(keys[0].action, KeyAction::Press);
        assert_eq!(keys[0].text.as_deref(), Some("a"));
    }

    #[test]
    fn kitty_csi_u_text_codepoints_multi_char() {
        let mut p = StdinParser::new();
        // KIP allows multiple text codepoints (e.g. combining marks).
        // CSI 97 ; 1 : 1 : 97 : 769 u — 'a' + U+0301 combining acute.
        let evs = p.feed(b"\x1b[97;1:1:97:769u");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].text.as_deref(), Some("a\u{0301}"));
    }

    #[test]
    fn kitty_csi_u_text_codepoints_skip_zero_entries() {
        let mut p = StdinParser::new();
        // Zero sub-params are "no associated text" markers — skip them.
        let evs = p.feed(b"\x1b[97;1:1:0u");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].text, None);
    }

    #[test]
    fn kitty_csi_u_text_codepoints_for_non_printable_keycode() {
        let mut p = StdinParser::new();
        // F1 (PUA 57364) with associated text payload "x" (codepoint 120).
        // Confirms text codepoints surface even when the keycode itself
        // maps to a functional (non-printable) PhysicalKey.
        let evs = p.feed(b"\x1b[57364;1:1:120u");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::F1);
        assert_eq!(keys[0].text.as_deref(), Some("x"));
    }

    #[test]
    fn kitty_csi_u_hyper_collapses_into_super() {
        let mut p = StdinParser::new();
        // mods = 1 + hyper(16) = 17
        let evs = p.feed(b"\x1b[97;17u");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::A);
        assert!(
            keys[0].mods.contains(ModSet::SUPER),
            "hyper must collapse into SUPER"
        );
    }

    #[test]
    fn kitty_csi_u_meta_collapses_into_alt() {
        let mut p = StdinParser::new();
        // mods = 1 + meta(32) = 33
        let evs = p.feed(b"\x1b[97;33u");
        let keys = key_only(&evs);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, PhysicalKey::A);
        assert!(
            keys[0].mods.contains(ModSet::ALT),
            "meta must collapse into ALT"
        );
    }

    #[test]
    fn kitty_csi_u_escape_and_enter() {
        let mut p = StdinParser::new();
        let esc = p.feed(b"\x1b[27u");
        assert_eq!(key_only(&esc)[0].key, PhysicalKey::Escape);
        let enter = p.feed(b"\x1b[13u");
        assert_eq!(key_only(&enter)[0].key, PhysicalKey::Enter);
    }

    #[test]
    fn kitty_csi_u_arrow_pua_codepoint() {
        let mut p = StdinParser::new();
        // ArrowUp = 57352
        let evs = p.feed(b"\x1b[57352u");
        let keys = key_only(&evs);
        assert_eq!(keys[0].key, PhysicalKey::ArrowUp);
    }

    #[test]
    fn kitty_csi_u_super_modifier() {
        let mut p = StdinParser::new();
        // mods = 1 + super(8) = 9
        let evs = p.feed(b"\x1b[97;9u");
        let keys = key_only(&evs);
        assert_eq!(keys[0].key, PhysicalKey::A);
        assert!(keys[0].mods.contains(ModSet::SUPER));
    }

    #[test]
    fn kitty_csi_u_empty_keycode_dropped() {
        let mut p = StdinParser::new();
        // CSI u with no params — keycode would be 0, must drop silently.
        let evs = p.feed(b"\x1b[u");
        assert!(key_only(&evs).is_empty());
        assert!(!p.has_pending());
    }

    #[test]
    fn kitty_modifier_code_table() {
        assert_eq!(kitty_modifier_code(1), ModSet::empty());
        assert_eq!(kitty_modifier_code(2), ModSet::SHIFT);
        assert_eq!(kitty_modifier_code(3), ModSet::ALT);
        assert_eq!(kitty_modifier_code(5), ModSet::CTRL);
        assert_eq!(kitty_modifier_code(9), ModSet::SUPER);
        assert_eq!(
            kitty_modifier_code(8),
            ModSet::SHIFT | ModSet::ALT | ModSet::CTRL
        );
        // Hyper collapses into SUPER, Meta collapses into ALT (v0 fallback —
        // see fn doc).
        assert_eq!(kitty_modifier_code(1 + 16), ModSet::SUPER);
        assert_eq!(kitty_modifier_code(1 + 32), ModSet::ALT);
        // Caps lock + num lock pass through.
        assert_eq!(kitty_modifier_code(1 + 64), ModSet::CAPS_LOCK);
        assert_eq!(kitty_modifier_code(1 + 128), ModSet::NUM_LOCK);
    }

    // ---- Sanity: existing "unknown CSI" path still recovers -------------

    #[test]
    fn xterm_modifier_code_table() {
        assert_eq!(xterm_modifier_code(1), ModSet::empty());
        assert_eq!(xterm_modifier_code(2), ModSet::SHIFT);
        assert_eq!(xterm_modifier_code(3), ModSet::ALT);
        assert_eq!(xterm_modifier_code(5), ModSet::CTRL);
        assert_eq!(xterm_modifier_code(9), ModSet::SUPER);
        assert_eq!(
            xterm_modifier_code(8),
            ModSet::SHIFT | ModSet::ALT | ModSet::CTRL
        );
    }
}
