//! Focus event handling with DEC 1004 gating.
//!
//! Per ADR-0008, `FocusEvent` is a direct re-export of libghostty's
//! `focus::Event`. There is no conversion layer; this module exists to
//! gate emission on the pane's DEC mode 1004 state and to own a reusable
//! encode buffer for `Event::encode`.

use libghostty_vt::{Error, Terminal as GhosttyTerminal, terminal::Mode};
use phux_protocol::input::focus::FocusEvent;

/// Per-pane focus encoder.
///
/// Holds a reusable byte buffer. Focus encoding itself is a stateless
/// libghostty free function (`Event::encode`); the type exists for API
/// symmetry with the other per-pane encoders and to own the byte buffer.
#[derive(Debug, Default)]
pub struct PerTerminalFocusEncoder {
    buf: Vec<u8>,
}

impl PerTerminalFocusEncoder {
    /// Construct a new per-pane focus encoder.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(8),
        }
    }

    /// Encode a focus event into PTY bytes — but only if the pane has DEC
    /// mode 1004 enabled.
    ///
    /// Returns `Ok(None)` if focus reporting is off (the event is silently
    /// dropped per SPEC §9.3), `Ok(Some(&[u8]))` with the report otherwise.
    pub fn encode(
        &mut self,
        event: FocusEvent,
        terminal: &GhosttyTerminal<'_, '_>,
    ) -> Result<Option<&[u8]>, Error> {
        self.encode_with_mode(event, terminal.mode(Mode::FOCUS_EVENT)?)
    }

    /// Encode from a snapshotted DEC 1004 focus-reporting mode.
    pub fn encode_with_mode(
        &mut self,
        event: FocusEvent,
        focus_reporting: bool,
    ) -> Result<Option<&[u8]>, Error> {
        if !focus_reporting {
            return Ok(None);
        }
        // Wire atom -> libghostty's focus::Event for the encoder (ADR-0024).
        let event = libghostty_vt::focus::Event::from(event);
        // 8 bytes is plenty for CSI I / CSI O (3 bytes each).
        self.buf.resize(8, 0);
        let written = loop {
            match event.encode(&mut self.buf) {
                Ok(n) => break n,
                Err(Error::OutOfSpace { required }) => {
                    self.buf.resize(required, 0);
                }
                Err(e) => return Err(e),
            }
        };
        self.buf.truncate(written);
        Ok(Some(&self.buf))
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
    fn encode_drops_when_mode_1004_off() {
        let terminal = make_terminal();
        let mut enc = PerTerminalFocusEncoder::new();
        let out = enc.encode(FocusEvent::Gained, &terminal).expect("encode");
        assert!(out.is_none(), "expected drop, got {out:?}");
    }

    #[test]
    fn encode_emits_csi_i_when_mode_1004_on() {
        let mut terminal = make_terminal();
        terminal
            .set_mode(Mode::FOCUS_EVENT, true)
            .expect("enable 1004");
        let mut enc = PerTerminalFocusEncoder::new();
        let bytes = enc
            .encode(FocusEvent::Gained, &terminal)
            .expect("encode")
            .expect("encoded payload");
        // CSI I = ESC [ I
        assert_eq!(
            bytes, b"\x1b[I",
            "unexpected focus-gained report: {bytes:?}"
        );
    }

    #[test]
    fn encode_emits_csi_o_for_lost_when_mode_on() {
        let mut terminal = make_terminal();
        terminal
            .set_mode(Mode::FOCUS_EVENT, true)
            .expect("enable 1004");
        let mut enc = PerTerminalFocusEncoder::new();
        let bytes = enc
            .encode(FocusEvent::Lost, &terminal)
            .expect("encode")
            .expect("encoded payload");
        assert_eq!(bytes, b"\x1b[O", "unexpected focus-lost report: {bytes:?}");
    }
}
