//! Paste event translation: wire → libghostty-vt with safety + bracketing.
//!
//! libghostty exposes paste as two free functions
//! (`paste::is_safe`, `paste::encode`) rather than a typed event, so there
//! is no libghostty struct to `From`-convert into. Instead, the wire
//! [`PasteEvent`] flows through [`PerTerminalPasteEncoder::encode`], which:
//!
//! 1. Classifies untrusted payloads with `paste::is_safe`.
//! 2. Rejects untrusted payloads that are not safe.
//! 3. Encodes via `paste::encode`, choosing the `bracketed` flag from the
//!    pane's DEC mode 2004 state.
//!
//! See `docs/spec/input.md` §5 and ADR-0006.

use libghostty_vt::{
    Error, Terminal as GhosttyTerminal,
    paste::{encode as paste_encode, is_safe},
    terminal::Mode,
};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};

/// Result of a paste encode attempt.
#[derive(Debug)]
pub enum PasteOutcome<'a> {
    /// Encoded bytes, ready to write to the PTY.
    Encoded(&'a [u8]),
    /// An untrusted payload failed `paste::is_safe`.
    Rejected,
}

/// Per-pane paste encoder.
#[derive(Debug)]
pub struct PerTerminalPasteEncoder {
    /// Reusable scratch buffer for `paste::encode`'s in-place input mutation.
    scratch: Vec<u8>,
    /// Reusable output buffer.
    out: Vec<u8>,
}

impl Default for PerTerminalPasteEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl PerTerminalPasteEncoder {
    /// Construct a new per-pane paste encoder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            scratch: Vec::new(),
            out: Vec::new(),
        }
    }

    /// Encode a wire paste event into PTY bytes.
    ///
    /// Returns [`PasteOutcome::Rejected`] for an untrusted-and-unsafe
    /// payload. Otherwise
    /// returns [`PasteOutcome::Encoded`] holding a slice into the encoder's
    /// internal buffer, valid until the next call.
    pub fn encode(
        &mut self,
        event: &PasteEvent,
        terminal: &GhosttyTerminal<'_, '_>,
    ) -> Result<PasteOutcome<'_>, Error> {
        self.encode_with_mode(event, terminal.mode(Mode::BRACKETED_PASTE)?)
    }

    /// Encode from a snapshotted DEC 2004 bracketed-paste mode.
    pub fn encode_with_mode(
        &mut self,
        event: &PasteEvent,
        bracketed: bool,
    ) -> Result<PasteOutcome<'_>, Error> {
        if Self::would_reject(event) {
            return Ok(PasteOutcome::Rejected);
        }

        // Copy the payload into the scratch buffer; `paste::encode` mutates
        // its input in place (strips control bytes / replaces newlines).
        self.scratch.clear();
        self.scratch.extend_from_slice(&event.data);

        // Conservative initial output buffer: input length + bracketed
        // paste sequence overhead (CSI ?2004h-style markers, ~13 bytes).
        let initial = self.scratch.len() + 16;
        self.out.resize(initial, 0);
        let written = loop {
            match paste_encode(&mut self.scratch, bracketed, &mut self.out) {
                Ok(n) => break n,
                Err(Error::OutOfSpace { required }) => {
                    self.out.resize(required, 0);
                }
                Err(e) => return Err(e),
            }
        };
        self.out.truncate(written);
        Ok(PasteOutcome::Encoded(&self.out))
    }

    /// Whether this paste is rejected before encoding.
    pub(crate) fn would_reject(event: &PasteEvent) -> bool {
        event.trust == PasteTrust::Untrusted
            // `is_safe` accepts UTF-8 only; arbitrary untrusted bytes are unsafe.
            && !std::str::from_utf8(&event.data).is_ok_and(is_safe)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn make_terminal() -> GhosttyTerminal<'static, 'static> {
        {
            let mut terminal = GhosttyTerminal::new(80, 24).expect("Terminal::new");
            terminal
                .set_scrollback_max_lines(Some(1000))
                .expect("Terminal::new");
            terminal
        }
    }

    #[test]
    fn trusted_paste_encodes_without_bracketing_when_mode_2004_off() {
        let terminal = make_terminal();
        let mut enc = PerTerminalPasteEncoder::new();
        let ev = PasteEvent {
            trust: PasteTrust::Trusted,
            data: b"hello".to_vec(),
        };
        let out = enc.encode(&ev, &terminal).expect("encode");
        match out {
            PasteOutcome::Encoded(b) => {
                // No bracket markers in non-bracketed mode.
                assert_eq!(b, b"hello", "unexpected payload {b:?}");
            }
            PasteOutcome::Rejected => panic!("trusted should not be rejected"),
        }
    }

    #[test]
    fn trusted_paste_brackets_when_mode_2004_on() {
        let mut terminal = make_terminal();
        terminal
            .set_mode(Mode::BRACKETED_PASTE, true)
            .expect("enable 2004");
        let mut enc = PerTerminalPasteEncoder::new();
        let ev = PasteEvent {
            trust: PasteTrust::Trusted,
            data: b"hi".to_vec(),
        };
        let out = enc.encode(&ev, &terminal).expect("encode");
        match out {
            PasteOutcome::Encoded(b) => {
                // Bracketed paste wraps payload with ESC [200~ ... ESC [201~.
                assert!(
                    b.starts_with(b"\x1b[200~") && b.ends_with(b"\x1b[201~"),
                    "expected bracketed-paste wrapping, got {b:?}"
                );
            }
            PasteOutcome::Rejected => panic!("trusted should not be rejected"),
        }
    }

    #[test]
    fn untrusted_unsafe_is_rejected_by_default() {
        let terminal = make_terminal();
        let mut enc = PerTerminalPasteEncoder::new();
        // Newline makes `is_safe` return false.
        let ev = PasteEvent {
            trust: PasteTrust::Untrusted,
            data: b"rm -rf /\n".to_vec(),
        };
        let out = enc.encode(&ev, &terminal).expect("encode");
        assert!(matches!(out, PasteOutcome::Rejected));
    }

    #[test]
    fn untrusted_safe_is_allowed_by_default() {
        let terminal = make_terminal();
        let mut enc = PerTerminalPasteEncoder::new();
        let ev = PasteEvent {
            trust: PasteTrust::Untrusted,
            data: b"safe payload".to_vec(),
        };
        let out = enc.encode(&ev, &terminal).expect("encode");
        assert!(matches!(out, PasteOutcome::Encoded(_)));
    }
}
