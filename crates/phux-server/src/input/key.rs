//! Wire `KeyEvent` → libghostty allocator-bound `key::Event` + per-pane encoder.
//!
//! Per ADR-0008, `KeyEvent`'s atoms (`KeyAction`, `PhysicalKey`, `ModSet`)
//! ARE libghostty's `key::Action`/`Key`/`Mods` — they're re-exports, not
//! mirrors. So this module no longer has enum-conversion functions; it just
//! composes libghostty's allocator-bound `Event` from the wire fields.
//!
//! [`PerTerminalKeyEncoder`] owns the per-pane `libghostty_vt::key::Encoder`
//! plus a reusable byte buffer — call [`PerTerminalKeyEncoder::encode`] with
//! the pane's current [`GhosttyTerminal`] and the wire event; the returned
//! `&[u8]` is the PTY payload.

use libghostty_vt::{
    Error, Terminal as GhosttyTerminal,
    key::{Encoder as LgKeyEncoder, Event as LgKeyEvent},
};
use phux_protocol::input::key::KeyEvent;

/// Build a libghostty `key::Event` from our wire `KeyEvent`.
///
/// Fallible only because libghostty's FFI allocator can fail; the field
/// copy itself is total. Returns a `'static`-allocator event ready for the
/// encoder.
pub fn key_event_to_libghostty(ev: &KeyEvent) -> Result<LgKeyEvent<'static>, Error> {
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

/// Per-pane key encoder.
///
/// Owns one `libghostty_vt::key::Encoder` plus a growable byte buffer
/// reused across calls. Per-pane: each pane should hold its own instance
/// so encoder state (KIP flags, modifyOtherKeys, etc.) reflects only that
/// pane's terminal state. See ADR-0006 §"Encoder options stay server-local".
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

    /// Encode a wire key event into PTY bytes.
    ///
    /// Refreshes encoder options from `terminal` (cursor key application
    /// mode, keypad mode, modifyOtherKeys, KIP flags, alt-esc-prefix,
    /// backarrow — see ADR-0006) before each encode so the bytes match
    /// what the inner program currently expects.
    ///
    /// Returns a slice borrowed from `self`'s internal buffer; the slice is
    /// valid until the next call to `encode`.
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

    #[test]
    fn encodes_plain_letter_a_to_byte_a() {
        let terminal = make_terminal();
        let mut enc = PerTerminalKeyEncoder::new().expect("encoder");
        let ev = KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::A,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: Some("a".to_owned()),
            unshifted_codepoint: Some(u32::from('a')),
        };
        let bytes = enc.encode(&ev, &terminal).expect("encode");
        assert!(
            bytes.starts_with(b"a"),
            "expected encoded bytes to start with `a`, got {bytes:?}"
        );
    }

    #[test]
    fn encodes_ctrl_j_to_line_feed() {
        // Ctrl+J must reach the PTY as LF (0x0A), distinct from Enter's CR. The
        // client parser produces this event from a 0x0A input byte.
        let terminal = make_terminal();
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
        let bytes = enc.encode(&ev, &terminal).expect("encode");
        assert_eq!(bytes, b"\n", "Ctrl+J must encode to LF, got {bytes:?}");
    }

    /// ADR-0024: the wire atoms are phux-owned but share libghostty's
    /// discriminants; the `server`-gated conversions are lossless for known
    /// values, keeping the two in lockstep.
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
}
