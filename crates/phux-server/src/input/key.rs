//! Wire `KeyEvent` -> libghostty `key::Event` plus a per-pane encoder. The
//! wire atoms are libghostty's own types (ADR-0008), so only composition
//! happens here.

use libghostty_vt::{
    Error, Terminal as GhosttyTerminal,
    key::{Encoder as LgKeyEncoder, EncoderOptions, Event as LgKeyEvent, KittyKeyFlags},
};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

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
    ///
    /// A legacy printable press or repeat is copied straight from `text`.
    /// That subset is byte-identical to libghostty and skips allocating a
    /// key event. Releases, modifiers, keypad keys, and any protocol that
    /// rewrites the byte (kitty, modifyOtherKeys) stay on libghostty.
    pub fn encode_with_options(
        &mut self,
        event: &KeyEvent,
        options: EncoderOptions,
    ) -> Result<&[u8], Error> {
        if let Some(text) = legacy_printable(event, options) {
            self.buf.clear();
            self.buf.extend_from_slice(text.as_bytes());
            return Ok(&self.buf);
        }
        let lg_event = key_event_to_libghostty(event)?;
        self.encoder.set_options(options);
        self.buf.clear();
        self.encoder.encode_to_vec(&lg_event, &mut self.buf)?;
        Ok(&self.buf)
    }
}

/// Lock bits that do not change the bytes of a legacy printable key.
const LOCK_MODS: ModSet =
    ModSet::from_bits_truncate(ModSet::CAPS_LOCK.bits() | ModSet::NUM_LOCK.bits());

/// UTF-8 libghostty would emit unchanged for a legacy press or repeat.
///
/// `None` means the caller must use the full encoder. The text is borrowed
/// from `event` and contains no C0, DEL, or macOS function-key PUA.
pub(crate) fn legacy_printable(event: &KeyEvent, options: EncoderOptions) -> Option<&str> {
    if event.composing || !matches!(event.action, KeyAction::Press | KeyAction::Repeat) {
        return None;
    }
    if options.kitty_flags != KittyKeyFlags::DISABLED
        || options.modify_other_keys_state_2
        || options.keypad_key_application
    {
        return None;
    }
    if !event.mods.difference(LOCK_MODS).is_empty()
        || !event.consumed_mods.difference(LOCK_MODS).is_empty()
    {
        return None;
    }
    let key = event.key as u32;
    if (PhysicalKey::Numpad0 as u32..=PhysicalKey::NumpadPageDown as u32).contains(&key) {
        return None;
    }
    let text = event.text.as_deref()?;
    if text.is_empty()
        || text.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
        || text
            .chars()
            .any(|ch| ('\u{F700}'..='\u{F8FF}').contains(&ch))
    {
        return None;
    }
    Some(text)
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

    fn plain(action: KeyAction, text: Option<&str>, mods: ModSet) -> KeyEvent {
        KeyEvent {
            action,
            key: PhysicalKey::A,
            mods,
            consumed_mods: ModSet::empty(),
            composing: false,
            text: text.map(str::to_owned),
            unshifted_codepoint: text.and_then(|value| value.chars().next()).map(u32::from),
        }
    }

    fn default_options() -> EncoderOptions {
        EncoderOptions {
            cursor_key_application: false,
            keypad_key_application: false,
            ignore_keypad_with_numlock: false,
            alt_esc_prefix: false,
            modify_other_keys_state_2: false,
            kitty_flags: KittyKeyFlags::DISABLED,
            backarrow_key_mode: false,
        }
    }

    fn libghostty_bytes(event: &KeyEvent, options: EncoderOptions) -> Vec<u8> {
        let mut encoder = LgKeyEncoder::new().expect("encoder");
        encoder.set_options(options);
        let lg = key_event_to_libghostty(event).expect("event");
        let mut buf = Vec::new();
        encoder.encode_to_vec(&lg, &mut buf).expect("encode");
        buf
    }

    #[test]
    fn legacy_printable_press_and_repeat_copy_the_text() {
        let options = default_options();
        for action in [KeyAction::Press, KeyAction::Repeat] {
            let event = plain(action, Some("a"), ModSet::empty());
            assert_eq!(legacy_printable(&event, options), Some("a"));
            let mut enc = PerTerminalKeyEncoder::new().expect("encoder");
            assert_eq!(
                enc.encode_with_options(&event, options).expect("encode"),
                b"a"
            );
        }
        let locked = plain(
            KeyAction::Press,
            Some("é"),
            ModSet::CAPS_LOCK | ModSet::NUM_LOCK,
        );
        assert_eq!(legacy_printable(&locked, options), Some("é"));
        let mut enc = PerTerminalKeyEncoder::new().expect("encoder");
        let encoded = enc.encode_with_options(&locked, options).expect("encode");
        assert_eq!(encoded, "é".as_bytes());
        assert_eq!(encoded, libghostty_bytes(&locked, options));
    }

    #[test]
    fn a_release_with_text_does_not_take_the_fast_path() {
        let options = default_options();
        let event = plain(KeyAction::Release, Some("a"), ModSet::empty());
        assert!(legacy_printable(&event, options).is_none());
        let mut enc = PerTerminalKeyEncoder::new().expect("encoder");
        assert_eq!(
            enc.encode_with_options(&event, options).expect("encode"),
            b""
        );
    }

    #[test]
    fn protocol_and_modifier_modes_stay_on_libghostty() {
        let event = plain(KeyAction::Press, Some("a"), ModSet::empty());
        let mut kitty = default_options();
        kitty.kitty_flags = KittyKeyFlags::DISAMBIGUATE;
        let mut modify = default_options();
        modify.modify_other_keys_state_2 = true;
        let mut keypad = default_options();
        keypad.keypad_key_application = true;
        let shifted = plain(KeyAction::Press, Some("A"), ModSet::SHIFT);
        let composing = KeyEvent {
            composing: true,
            ..plain(KeyAction::Press, Some("'"), ModSet::empty())
        };
        let numpad = KeyEvent {
            key: PhysicalKey::Numpad1,
            ..plain(KeyAction::Press, Some("1"), ModSet::NUM_LOCK)
        };
        for (name, options, event) in [
            ("kitty", kitty, event.clone()),
            ("modifyOtherKeys", modify, event.clone()),
            ("keypad", keypad, event),
            ("shift", default_options(), shifted),
            ("composing", default_options(), composing),
            ("numpad", default_options(), numpad),
        ] {
            assert!(
                legacy_printable(&event, options).is_none(),
                "{name} must stay on libghostty"
            );
            let mut enc = PerTerminalKeyEncoder::new().expect("encoder");
            assert_eq!(
                enc.encode_with_options(&event, options)
                    .expect("encode")
                    .to_vec(),
                libghostty_bytes(&event, options),
                "{name}"
            );
        }
    }

    #[test]
    fn kitty_push_does_not_take_the_printable_fast_path() {
        let mut terminal = GhosttyTerminal::new(80, 24).expect("terminal");
        terminal.vt_write(b"\x1b[>1u");
        let options = EncoderOptions::from_terminal(&terminal).expect("options");
        let event = plain(KeyAction::Press, Some("a"), ModSet::empty());
        assert!(legacy_printable(&event, options).is_none());
        let mut enc = PerTerminalKeyEncoder::new().expect("encoder");
        assert_eq!(
            enc.encode(&event, &terminal).expect("encode").to_vec(),
            libghostty_bytes(&event, options)
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
