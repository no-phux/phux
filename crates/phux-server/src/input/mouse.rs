//! Wire `MouseEvent` -> libghostty `mouse::Event` plus a per-pane encoder.
//! The wire atoms are libghostty's own types (ADR-0008);
//! [`MouseButton::Unknown`] is the wire's "no button" sentinel for motion.

use libghostty_vt::{
    Error, Terminal as GhosttyTerminal,
    mouse::{
        Encoder as LgMouseEncoder, EncoderSize, Event as LgMouseEvent, Position as LgMousePosition,
    },
};
use phux_protocol::input::mouse::{MouseButton, MouseEvent};

/// libghostty takes `None` for the wire's `Unknown` "no button" sentinel.
const fn option_for_encoder(button: MouseButton) -> Option<MouseButton> {
    match button {
        MouseButton::Unknown => None,
        other => Some(other),
    }
}

/// Build a libghostty `mouse::Event` from a wire `MouseEvent`, downcasting
/// the `f64` surface position to libghostty's `f32`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "libghostty's surface coords are f32"
)]
fn mouse_event_to_libghostty(ev: &MouseEvent) -> Result<LgMouseEvent<'static>, Error> {
    let mut out = LgMouseEvent::new()?;
    out.set_action(ev.action.into())
        .set_button(option_for_encoder(ev.button).map(Into::into))
        .set_mods(ev.mods.into())
        .set_position(LgMousePosition {
            x: ev.x as f32,
            y: ev.y as f32,
        });
    Ok(out)
}

/// Per-pane mouse encoder: tracking mode, format, and motion-dedupe state
/// reflect one pane's terminal.
#[derive(Debug)]
pub struct PerTerminalMouseEncoder {
    encoder: LgMouseEncoder<'static>,
    buf: Vec<u8>,
}

impl PerTerminalMouseEncoder {
    /// Construct a new per-pane mouse encoder.
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            encoder: LgMouseEncoder::new()?,
            buf: Vec::with_capacity(32),
        })
    }

    /// Encode a wire mouse event into PTY bytes, with tracking mode and
    /// format refreshed from `terminal`.
    ///
    /// The encoder converts surface pixels to cells through the grid size
    /// and `cell_px` (SPEC input.md §3.2); zero geometry would encode every
    /// event to nothing, so each axis is clamped to at least 1px.
    pub fn encode(
        &mut self,
        event: &MouseEvent,
        terminal: &GhosttyTerminal<'_, '_>,
        cell_px: (u16, u16),
    ) -> Result<&[u8], Error> {
        let options = libghostty_vt::mouse::EncoderOptions::from_terminal(terminal)?;
        self.encode_with_options(event, options, terminal.cols()?, terminal.rows()?, cell_px)
    }

    /// Encode from exact terminal-derived mode and geometry snapshots.
    pub fn encode_with_options(
        &mut self,
        event: &MouseEvent,
        options: libghostty_vt::mouse::EncoderOptions,
        cols: u16,
        rows: u16,
        cell_px: (u16, u16),
    ) -> Result<&[u8], Error> {
        let lg_event = mouse_event_to_libghostty(event)?;
        let cell_width = u32::from(cell_px.0.max(1));
        let cell_height = u32::from(cell_px.1.max(1));
        self.encoder.set_options(options).set_size(EncoderSize {
            screen_width: u32::from(cols).saturating_mul(cell_width),
            screen_height: u32::from(rows).saturating_mul(cell_height),
            cell_width,
            cell_height,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        });
        self.buf.clear();
        self.encoder.encode_to_vec(&lg_event, &mut self.buf)?;
        Ok(&self.buf)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use phux_protocol::input::key::ModSet;
    use phux_protocol::input::mouse::MouseAction;

    #[test]
    fn mouse_event_to_libghostty_round_trips_fields() {
        let ev = MouseEvent {
            action: MouseAction::Press,
            button: MouseButton::Left,
            mods: ModSet::SHIFT,
            x: 12.5,
            y: 34.25,
        };
        let lg = mouse_event_to_libghostty(&ev).expect("convert");
        assert_eq!(lg.action(), MouseAction::Press.into());
        assert_eq!(lg.button(), Some(MouseButton::Left.into()));
        assert_eq!(lg.mods(), ModSet::SHIFT.into());
        let pos = lg.position();
        assert!((pos.x - 12.5_f32).abs() < f32::EPSILON);
        assert!((pos.y - 34.25_f32).abs() < f32::EPSILON);
        let motion = MouseEvent {
            button: MouseButton::Unknown,
            ..ev
        };
        assert_eq!(
            mouse_event_to_libghostty(&motion)
                .expect("convert")
                .button(),
            None
        );
    }

    /// phux-yyex regression: without cell geometry libghostty encoded every
    /// mouse event to nothing. Clicks and wheel encode as SGR, zero geometry
    /// clamps to 1px, and an untracked terminal still encodes nothing.
    /// `(modes, button, x, y, cell_px, expected bytes)`.
    type Case = (
        &'static [u8],
        MouseButton,
        f64,
        f64,
        (u16, u16),
        &'static [u8],
    );

    #[test]
    fn mouse_encoding_table() {
        const SGR: &[u8] = b"\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h";
        // Cell (10, 5) at 8x16px cells is surface (80, 80): 1-based (11, 6).
        let cases: [Case; 5] = [
            (
                SGR,
                MouseButton::Left,
                80.0,
                80.0,
                (8, 16),
                b"\x1b[<0;11;6M",
            ),
            (
                SGR,
                MouseButton::Four,
                80.0,
                80.0,
                (8, 16),
                b"\x1b[<64;11;6M",
            ),
            (
                SGR,
                MouseButton::Five,
                80.0,
                80.0,
                (8, 16),
                b"\x1b[<65;11;6M",
            ),
            (
                b"\x1b[?1000h\x1b[?1006h",
                MouseButton::Left,
                10.0,
                5.0,
                (0, 0),
                b"\x1b[<0;11;6M",
            ),
            (b"", MouseButton::Left, 80.0, 80.0, (8, 16), b""),
        ];
        for (modes, button, x, y, cell_px, want) in cases {
            let mut terminal = GhosttyTerminal::new(80, 24).expect("Terminal::new");
            terminal.vt_write(modes);
            let event = MouseEvent {
                action: MouseAction::Press,
                button,
                mods: ModSet::empty(),
                x,
                y,
            };
            let mut enc = PerTerminalMouseEncoder::new().expect("encoder");
            let got = enc.encode(&event, &terminal, cell_px).expect("encode");
            assert_eq!(got, want, "{button:?} at ({x}, {y}) with {cell_px:?}");
        }
    }
}
