//! Paste encoding: untrusted payloads that fail `paste::is_safe` are
//! rejected, and the rest are encoded with bracketing chosen by the pane's
//! DEC 2004 mode (docs/spec/input.md §5, ADR-0006).

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

    fn encode(trust: PasteTrust, data: &[u8], bracketed: bool) -> Option<Vec<u8>> {
        let mut terminal = GhosttyTerminal::new(80, 24).expect("Terminal::new");
        terminal
            .set_mode(Mode::BRACKETED_PASTE, bracketed)
            .expect("mode 2004");
        let event = PasteEvent {
            trust,
            data: data.to_vec(),
        };
        match PerTerminalPasteEncoder::new()
            .encode(&event, &terminal)
            .expect("encode")
        {
            PasteOutcome::Encoded(bytes) => Some(bytes.to_vec()),
            PasteOutcome::Rejected => None,
        }
    }

    #[test]
    fn paste_trust_and_bracketing_table() {
        use PasteTrust::{Trusted, Untrusted};
        assert_eq!(
            encode(Trusted, b"hello", false).as_deref(),
            Some(&b"hello"[..])
        );
        assert_eq!(
            encode(Trusted, b"hi", true).as_deref(),
            Some(&b"\x1b[200~hi\x1b[201~"[..])
        );
        // A newline makes an untrusted payload unsafe.
        assert_eq!(encode(Untrusted, b"rm -rf /\n", false), None);
        assert!(encode(Untrusted, b"safe payload", false).is_some());
    }
}
