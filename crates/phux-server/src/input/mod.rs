//! Server-side input plumbing: wire events -> libghostty-vt events -> PTY
//! bytes (docs/spec/input.md, ADR-0006, ADR-0008).
//!
//! The wire's input atoms are libghostty's own types, so this layer only
//! composes libghostty's allocator-bound events, gates emission on terminal
//! modes (DEC 1004 focus, DEC 2004 bracketed paste), rejects unsafe untrusted
//! pastes, and owns one stateful encoder set per pane.

pub mod focus;
pub mod key;
pub mod mouse;
pub mod paste;

pub use focus::PerTerminalFocusEncoder;
pub use key::PerTerminalKeyEncoder;
pub use mouse::PerTerminalMouseEncoder;
pub use paste::{PasteOutcome, PerTerminalPasteEncoder};

use libghostty_vt::{Error, Terminal as GhosttyTerminal, terminal::Mode};

/// Complete `Send` snapshot of terminal state consulted by input encoders.
///
/// Captured only by the pane actor that owns the `!Send` terminal, then
/// published to the dedicated input lane. Key and mouse options come from
/// libghostty's exact terminal-derived capture API, so mode precedence remains
/// owned by libghostty rather than duplicated here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputEncoderSnapshot {
    /// Terminal-derived key encoder options.
    pub key: libghostty_vt::key::EncoderOptions,
    /// Effective terminal-derived mouse tracking and format options.
    pub mouse: libghostty_vt::mouse::EncoderOptions,
    /// DEC 1004 focus reporting.
    pub focus_reporting: bool,
    /// DEC 2004 bracketed paste.
    pub bracketed_paste: bool,
    /// Grid width in cells.
    pub cols: u16,
    /// Grid height in cells.
    pub rows: u16,
    /// Cell pixel dimensions.
    pub cell_px: (u16, u16),
}

impl Default for InputEncoderSnapshot {
    fn default() -> Self {
        Self {
            key: libghostty_vt::key::EncoderOptions {
                cursor_key_application: false,
                keypad_key_application: false,
                ignore_keypad_with_numlock: false,
                alt_esc_prefix: false,
                modify_other_keys_state_2: false,
                kitty_flags: libghostty_vt::key::KittyKeyFlags::DISABLED,
                backarrow_key_mode: false,
            },
            mouse: libghostty_vt::mouse::EncoderOptions {
                tracking_mode: libghostty_vt::mouse::TrackingMode::None,
                format: libghostty_vt::mouse::Format::X10,
            },
            focus_reporting: false,
            bracketed_paste: false,
            cols: 80,
            rows: 24,
            cell_px: (8, 16),
        }
    }
}

impl InputEncoderSnapshot {
    /// Capture every terminal mode and dimension read by key, mouse, focus,
    /// and paste encoding.
    pub fn capture(terminal: &GhosttyTerminal<'_, '_>, cell_px: (u16, u16)) -> Result<Self, Error> {
        Ok(Self {
            key: libghostty_vt::key::EncoderOptions::from_terminal(terminal)?,
            mouse: libghostty_vt::mouse::EncoderOptions::from_terminal(terminal)?,
            focus_reporting: terminal.mode(Mode::FOCUS_EVENT)?,
            bracketed_paste: terminal.mode(Mode::BRACKETED_PASTE)?,
            cols: terminal.cols()?,
            rows: terminal.rows()?,
            cell_px,
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod snapshot_tests {
    use super::*;
    use phux_protocol::input::{
        focus::FocusEvent,
        key::{KeyAction, KeyEvent, ModSet, PhysicalKey},
        mouse::{MouseAction, MouseButton, MouseEvent},
        paste::{PasteEvent, PasteTrust},
    };

    fn paste_bytes(outcome: &PasteOutcome<'_>) -> Option<Vec<u8>> {
        match outcome {
            PasteOutcome::Encoded(bytes) => Some(bytes.to_vec()),
            PasteOutcome::Rejected => None,
        }
    }

    /// The input lane encodes from a published snapshot rather than the
    /// live terminal; both must produce identical bytes, with every mode on
    /// and with none.
    #[test]
    fn live_terminal_and_snapshot_encoders_are_byte_identical() {
        let all_modes: &[u8] = b"\x1b[?1h\x1b[?66h\x1b[?1035h\x1b[?1036h\x1b[?67h\x1b[>4;2m\x1b[>31u\x1b[?1003h\x1b[?1006h\x1b[?1004h\x1b[?2004h";
        for (modes, mods, focus) in [
            (all_modes, ModSet::ALT, FocusEvent::Gained),
            (b"".as_slice(), ModSet::empty(), FocusEvent::Lost),
        ] {
            let mut terminal = GhosttyTerminal::new(91, 37).expect("terminal");
            terminal.vt_write(modes);
            let snapshot = InputEncoderSnapshot::capture(&terminal, (9, 17)).expect("snapshot");
            assert_eq!(
                (snapshot.cols, snapshot.rows, snapshot.cell_px),
                (91, 37, (9, 17))
            );
            assert_eq!(snapshot.focus_reporting, !modes.is_empty());
            assert_eq!(snapshot.bracketed_paste, !modes.is_empty());

            let key = KeyEvent {
                action: KeyAction::Press,
                key: PhysicalKey::ArrowUp,
                mods,
                consumed_mods: ModSet::empty(),
                composing: false,
                text: None,
                unshifted_codepoint: None,
            };
            let mut live = PerTerminalKeyEncoder::new().expect("key");
            let mut snap = PerTerminalKeyEncoder::new().expect("key");
            assert_eq!(
                live.encode(&key, &terminal).expect("live").to_vec(),
                snap.encode_with_options(&key, snapshot.key)
                    .expect("snap")
                    .to_vec(),
            );

            let mouse = MouseEvent {
                action: MouseAction::Press,
                button: MouseButton::Left,
                mods,
                x: 81.0,
                y: 85.0,
            };
            let mut live = PerTerminalMouseEncoder::new().expect("mouse");
            let mut snap = PerTerminalMouseEncoder::new().expect("mouse");
            assert_eq!(
                live.encode(&mouse, &terminal, snapshot.cell_px)
                    .expect("live")
                    .to_vec(),
                snap.encode_with_options(
                    &mouse,
                    snapshot.mouse,
                    snapshot.cols,
                    snapshot.rows,
                    snapshot.cell_px,
                )
                .expect("snap")
                .to_vec(),
            );

            let mut live = PerTerminalFocusEncoder::new();
            let mut snap = PerTerminalFocusEncoder::new();
            assert_eq!(
                live.encode(focus, &terminal)
                    .expect("live")
                    .map(<[u8]>::to_vec),
                snap.encode_with_mode(focus, snapshot.focus_reporting)
                    .expect("snap")
                    .map(<[u8]>::to_vec),
            );

            let paste = PasteEvent {
                trust: PasteTrust::Trusted,
                data: b"snapshot".to_vec(),
            };
            let mut live = PerTerminalPasteEncoder::new();
            let mut snap = PerTerminalPasteEncoder::new();
            let live = paste_bytes(&live.encode(&paste, &terminal).expect("live"));
            let snap = paste_bytes(
                &snap
                    .encode_with_mode(&paste, snapshot.bracketed_paste)
                    .expect("snap"),
            );
            assert!(live.is_some());
            assert_eq!(live, snap);
        }
    }
}
