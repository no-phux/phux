//! Server-side input plumbing: wire events → libghostty-vt events → PTY bytes.
//!
//! Per ADR-0008, the wire's input atoms (`KeyAction`, `PhysicalKey`,
//! `ModSet`, `MouseAction`, `MouseButton`, `FocusEvent`) ARE libghostty's
//! types. The only work this layer does is:
//!
//! 1. Compose libghostty's allocator-bound `Event` types from the wire's
//!    plain field shapes (`KeyEvent`, `MouseEvent`).
//! 2. Gate emission on the terminal's state (focus mode 1004,
//!    bracketed-paste mode 2004).
//! 3. Apply per-terminal policy for untrusted paste payloads.
//! 4. Own one lane-local `PerTerminal*Encoder` per generational pane so
//!    state stays private to that terminal (ADR-0006, ADR-0044).
//!
//! See docs/spec/input.md and ADR-0006 + ADR-0008.

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
    use libghostty_vt::terminal::Mode;
    use phux_protocol::input::{
        focus::FocusEvent,
        key::{KeyAction, KeyEvent, ModSet, PhysicalKey},
        mouse::{MouseAction, MouseButton, MouseEvent},
        paste::{PasteEvent, PasteTrust},
    };

    fn terminal(modes: &[u8]) -> GhosttyTerminal<'static, 'static> {
        let mut terminal = {
            let mut terminal = GhosttyTerminal::new(91, 37).expect("terminal");
            terminal
                .set_scrollback_max_lines(Some(0))
                .expect("terminal");
            terminal
        };
        terminal.vt_write(modes);
        terminal
    }

    #[test]
    fn live_terminal_and_snapshot_encoders_are_byte_identical() {
        let terminal = terminal(
            b"\x1b[?1h\x1b[?66h\x1b[?1035h\x1b[?1036h\x1b[?67h\x1b[>4;2m\x1b[>31u\x1b[?1003h\x1b[?1006h\x1b[?1004h\x1b[?2004h",
        );
        let snapshot = InputEncoderSnapshot::capture(&terminal, (9, 17)).expect("snapshot");

        let key = KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::ArrowUp,
            mods: ModSet::ALT,
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        };
        let mut live_key = PerTerminalKeyEncoder::new().expect("live key");
        let mut snap_key = PerTerminalKeyEncoder::new().expect("snapshot key");
        assert_eq!(
            live_key.encode(&key, &terminal).expect("live").to_vec(),
            snap_key
                .encode_with_options(&key, snapshot.key)
                .expect("snapshot")
                .to_vec(),
        );

        let mouse = MouseEvent {
            action: MouseAction::Press,
            button: MouseButton::Left,
            mods: ModSet::SHIFT,
            x: 81.0,
            y: 85.0,
        };
        let mut live_mouse = PerTerminalMouseEncoder::new().expect("live mouse");
        let mut snap_mouse = PerTerminalMouseEncoder::new().expect("snapshot mouse");
        assert_eq!(
            live_mouse
                .encode(&mouse, &terminal, snapshot.cell_px)
                .expect("live")
                .to_vec(),
            snap_mouse
                .encode_with_options(
                    &mouse,
                    snapshot.mouse,
                    snapshot.cols,
                    snapshot.rows,
                    snapshot.cell_px,
                )
                .expect("snapshot")
                .to_vec(),
        );

        let mut live_focus = PerTerminalFocusEncoder::new();
        let mut snap_focus = PerTerminalFocusEncoder::new();
        assert_eq!(
            live_focus
                .encode(FocusEvent::Gained, &terminal)
                .expect("live")
                .map(<[u8]>::to_vec),
            snap_focus
                .encode_with_mode(FocusEvent::Gained, snapshot.focus_reporting)
                .expect("snapshot")
                .map(<[u8]>::to_vec),
        );

        let paste = PasteEvent {
            trust: PasteTrust::Trusted,
            data: b"snapshot".to_vec(),
        };
        let mut live_paste = PerTerminalPasteEncoder::new();
        let mut snap_paste = PerTerminalPasteEncoder::new();
        let live = match live_paste.encode(&paste, &terminal).expect("live") {
            PasteOutcome::Encoded(bytes) => Some(bytes.to_vec()),
            PasteOutcome::Rejected => None,
        };
        let snap = match snap_paste
            .encode_with_mode(&paste, snapshot.bracketed_paste)
            .expect("snapshot")
        {
            PasteOutcome::Encoded(bytes) => Some(bytes.to_vec()),
            PasteOutcome::Rejected => None,
        };
        assert_eq!(live, snap);
    }

    #[test]
    fn disabled_mode_snapshot_encoding_matches_live_terminal() {
        let terminal = terminal(b"");
        let snapshot = InputEncoderSnapshot::capture(&terminal, (7, 13)).expect("snapshot");

        let key = KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::ArrowUp,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        };
        let mut live_key = PerTerminalKeyEncoder::new().expect("live key");
        let mut snap_key = PerTerminalKeyEncoder::new().expect("snapshot key");
        assert_eq!(
            live_key.encode(&key, &terminal).expect("live").to_vec(),
            snap_key
                .encode_with_options(&key, snapshot.key)
                .expect("snapshot")
                .to_vec(),
        );

        let mouse = MouseEvent {
            action: MouseAction::Press,
            button: MouseButton::Left,
            mods: ModSet::empty(),
            x: 14.0,
            y: 26.0,
        };
        let mut live_mouse = PerTerminalMouseEncoder::new().expect("live mouse");
        let mut snap_mouse = PerTerminalMouseEncoder::new().expect("snapshot mouse");
        assert_eq!(
            live_mouse
                .encode(&mouse, &terminal, snapshot.cell_px)
                .expect("live")
                .to_vec(),
            snap_mouse
                .encode_with_options(
                    &mouse,
                    snapshot.mouse,
                    snapshot.cols,
                    snapshot.rows,
                    snapshot.cell_px,
                )
                .expect("snapshot")
                .to_vec(),
        );

        let mut live_focus = PerTerminalFocusEncoder::new();
        let mut snap_focus = PerTerminalFocusEncoder::new();
        assert_eq!(
            live_focus
                .encode(FocusEvent::Lost, &terminal)
                .expect("live")
                .map(<[u8]>::to_vec),
            snap_focus
                .encode_with_mode(FocusEvent::Lost, snapshot.focus_reporting)
                .expect("snapshot")
                .map(<[u8]>::to_vec),
        );

        let paste = PasteEvent {
            trust: PasteTrust::Trusted,
            data: b"plain".to_vec(),
        };
        let mut live_paste = PerTerminalPasteEncoder::new();
        let mut snap_paste = PerTerminalPasteEncoder::new();
        let PasteOutcome::Encoded(live) = live_paste.encode(&paste, &terminal).expect("live")
        else {
            panic!("trusted live paste rejected");
        };
        let PasteOutcome::Encoded(snap) = snap_paste
            .encode_with_mode(&paste, snapshot.bracketed_paste)
            .expect("snapshot")
        else {
            panic!("trusted snapshot paste rejected");
        };
        assert_eq!(live, snap);
    }

    #[test]
    fn snapshot_tracks_disabled_focus_and_paste_and_exact_dimensions() {
        let mut terminal = terminal(b"");
        terminal
            .set_mode(Mode::FOCUS_EVENT, false)
            .expect("focus off");
        terminal
            .set_mode(Mode::BRACKETED_PASTE, false)
            .expect("paste off");
        let snapshot = InputEncoderSnapshot::capture(&terminal, (7, 13)).expect("snapshot");
        assert!(!snapshot.focus_reporting);
        assert!(!snapshot.bracketed_paste);
        assert_eq!((snapshot.cols, snapshot.rows), (91, 37));
        assert_eq!(snapshot.cell_px, (7, 13));
    }
}
