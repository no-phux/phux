//! Wire `KeyEvent` -> libghostty `key::Event` plus a per-pane encoder. The
//! wire atoms are libghostty's own types (ADR-0008), so only composition
//! happens here.

use libghostty_vt::{
    Error, Terminal as GhosttyTerminal,
    key::{Encoder as LgKeyEncoder, Event as LgKeyEvent},
};
use phux_protocol::input::key::KeyEvent;

/// Build a libghostty `key::Event` from a wire `KeyEvent`; fallible only
/// through libghostty's allocator.
fn key_event_to_libghostty(ev: &KeyEvent) -> Result<LgKeyEvent<'static>, Error> {
    let mut out = LgKeyEvent::new()?;
    out.set_action(ev.action.into())
        .set_key(ev.key.into())
        .set_mods(ev.mods.into())
        .set_consumed_mods(ev.consumed_mods.into())
        .set_composing(ev.composing)
        .set_utf8(ev.text.clone());
    if let Some(cp) = ev.unshifted_codepoint
        && let Some(ch) = char::from_u32(cp)
    {
        out.set_unshifted_codepoint(ch);
    }
    Ok(out)
}

/// Per-pane key encoder: one libghostty encoder plus a reused buffer, so
/// encoder state reflects only that pane's terminal (ADR-0006).
#[derive(Debug)]
pub struct PerTerminalKeyEncoder {
    encoder: LgKeyEncoder<'static>,
    buf: Vec<u8>,
}

impl PerTerminalKeyEncoder {
    /// Construct a new per-pane key encoder with a fresh libghostty allocator.
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            encoder: LgKeyEncoder::new()?,
            buf: Vec::with_capacity(32),
        })
    }

    /// Encode a wire key event into PTY bytes, with options refreshed from
    /// `terminal`'s current modes. The slice is valid until the next call.
    pub fn encode(
        &mut self,
        event: &KeyEvent,
        terminal: &GhosttyTerminal<'_, '_>,
    ) -> Result<&[u8], Error> {
        let options = libghostty_vt::key::EncoderOptions::from_terminal(terminal)?;
        self.encode_with_options(event, options)
    }

    /// Encode from an exact terminal-derived `Send` option snapshot.
    pub fn encode_with_options(
        &mut self,
        event: &KeyEvent,
        options: libghostty_vt::key::EncoderOptions,
    ) -> Result<&[u8], Error> {
        let lg_event = key_event_to_libghostty(event)?;
        self.encoder.set_options(options);
        self.buf.clear();
        self.encoder.encode_to_vec(&lg_event, &mut self.buf)?;
        Ok(&self.buf)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use libghostty_vt::key::{Action, Key, Mods};
    use phux_protocol::input::key::{KeyAction, ModSet, PhysicalKey};

    #[test]
    fn key_event_to_libghostty_round_trips_fields() {
        let ev = KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::A,
            mods: ModSet::CTRL | ModSet::SHIFT,
            consumed_mods: ModSet::SHIFT,
            composing: false,
            text: Some("A".to_owned()),
            unshifted_codepoint: Some(u32::from('a')),
        };
        let mut lg = key_event_to_libghostty(&ev).expect("convert");
        assert_eq!(lg.action(), Action::Press);
        assert_eq!(lg.key(), Key::A);
        assert_eq!(lg.mods(), Mods::CTRL | Mods::SHIFT);
        assert_eq!(lg.consumed_mods(), Mods::SHIFT);
        assert!(!lg.is_composing());
        assert_eq!(lg.utf8(), Some("A"));
        assert_eq!(lg.unshifted_codepoint(), 'a');
    }

    /// ADR-0024: the phux-owned wire atoms stay in lockstep with libghostty's.
    #[test]
    fn atoms_round_trip_libghostty() {
        for (pa, la) in [
            (KeyAction::Press, Action::Press),
            (KeyAction::Release, Action::Release),
            (KeyAction::Repeat, Action::Repeat),
        ] {
            assert_eq!(Action::from(pa), la);
            assert_eq!(KeyAction::from(la), pa);
        }
        assert_eq!(Key::from(PhysicalKey::A), Key::A);
        assert_eq!(PhysicalKey::from(Key::A), PhysicalKey::A);
        assert_eq!(
            Mods::from(ModSet::CTRL | ModSet::SHIFT),
            Mods::CTRL | Mods::SHIFT
        );
    }

    /// Ctrl+J must reach the PTY as LF (0x0A), distinct from Enter's CR.
    #[test]
    fn encodes_ctrl_j_to_line_feed() {
        let terminal = GhosttyTerminal::new(80, 24).expect("Terminal::new");
        let mut enc = PerTerminalKeyEncoder::new().expect("encoder");
        let ev = KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::J,
            mods: ModSet::CTRL,
            consumed_mods: ModSet::CTRL,
            composing: false,
            text: None,
            unshifted_codepoint: Some(u32::from('j')),
        };
        assert_eq!(enc.encode(&ev, &terminal).expect("encode"), b"\n");
    }
}
