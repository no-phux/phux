//! Focus reports, gated on the pane's DEC 1004 mode. `FocusEvent` is
//! libghostty's own type (ADR-0008).

use libghostty_vt::{Error, Terminal as GhosttyTerminal, terminal::Mode};
use phux_protocol::input::focus::FocusEvent;

/// Per-pane focus encoder; owns the reusable output buffer.
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

    /// Encode a focus event into PTY bytes; `Ok(None)` when DEC 1004 is off
    /// (dropped per SPEC §9.3).
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

    #[test]
    fn focus_reports_only_under_mode_1004() {
        let mut terminal = GhosttyTerminal::new(80, 24).expect("Terminal::new");
        let mut enc = PerTerminalFocusEncoder::new();
        assert!(
            enc.encode(FocusEvent::Gained, &terminal)
                .expect("encode")
                .is_none()
        );
        terminal
            .set_mode(Mode::FOCUS_EVENT, true)
            .expect("enable 1004");
        for (event, want) in [
            (FocusEvent::Gained, b"\x1b[I"),
            (FocusEvent::Lost, b"\x1b[O"),
        ] {
            let bytes = enc.encode(event, &terminal).expect("encode");
            assert_eq!(bytes, Some(want.as_slice()));
        }
    }
}
