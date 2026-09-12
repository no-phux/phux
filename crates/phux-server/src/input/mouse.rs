//! Wire `MouseEvent` → libghostty allocator-bound `mouse::Event` + per-pane encoder.
//!
//! Per ADR-0008, `MouseAction` and `MouseButton` are re-exports of
//! libghostty's `mouse::{Action, Button}`. Composition is the only work
//! here; no enum conversions.
//!
//! The wire form treats [`MouseButton::Unknown`] as the "no button"
//! sentinel for naked motion. libghostty's encoder takes `Option<Button>`,
//! so this module wraps each call site with `option_for_encoder`.

use libghostty_vt::{
    Error, Terminal as GhosttyTerminal,
    mouse::{
        Encoder as LgMouseEncoder, EncoderSize, Event as LgMouseEvent, Position as LgMousePosition,
    },
};
use phux_protocol::input::mouse::{MouseButton, MouseEvent};

/// Treat [`MouseButton::Unknown`] as the wire sentinel for "no button" and
/// hand libghostty `None` for it. Every other variant flows through
/// verbatim (they're the same type).
#[must_use]
pub const fn option_for_encoder(button: MouseButton) -> Option<MouseButton> {
    match button {
        MouseButton::Unknown => None,
        other => Some(other),
    }
}

/// Build a libghostty `mouse::Event` from our wire `MouseEvent`.
///
/// Fallibility comes only from libghostty's FFI allocator. Wire positions
/// are `f64`; libghostty's `MousePosition` is `f32` — downcast here. (That
/// surface-pixel precision is ample for terminal cell geometry.)
#[allow(
    clippy::cast_possible_truncation,
    reason = "f64 → f32 downcast is by design — libghostty's surface coords are f32"
)]
pub fn mouse_event_to_libghostty(ev: &MouseEvent) -> Result<LgMouseEvent<'static>, Error> {
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

/// Per-pane mouse encoder.
///
/// Wraps one `libghostty_vt::mouse::Encoder` plus a reusable byte buffer.
/// Per-pane: tracking mode, output format, and motion-deduplication state
/// reflect a single pane's terminal modes.
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

    /// Encode a wire mouse event into PTY bytes.
    ///
    /// Refreshes tracking-mode and format from `terminal` before each call
    /// so the encoded sequence matches whatever the inner program currently
    /// has enabled, and rebuilds `EncoderSize` from the terminal's grid plus
    /// `cell_px` (per-cell pixel size, SPEC input.md §3.2). The size is NOT
    /// optional: libghostty's encoder converts surface-space pixel positions
    /// to cells through it, and with zero cell geometry it encodes every
    /// event — clicks and wheel alike — to zero bytes (phux-yyex).
    ///
    /// `cell_px` axes are clamped to a 1px minimum so a degenerate resize can
    /// never regress to the encode-to-nothing state.
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
    }

    #[test]
    fn unknown_button_maps_to_none() {
        let ev = MouseEvent {
            action: MouseAction::Motion,
            button: MouseButton::Unknown,
            mods: ModSet::empty(),
            x: 0.0,
            y: 0.0,
        };
        let lg = mouse_event_to_libghostty(&ev).expect("convert");
        assert_eq!(lg.button(), None);
    }

    use libghostty_vt::Terminal;

    /// 80x24 terminal that has already applied `modes` (raw VT bytes).
    fn terminal_with(modes: &[u8]) -> Terminal<'static, 'static> {
        let mut t = {
            let mut terminal = Terminal::new(80, 24).expect("Terminal::new");
            terminal
                .set_scrollback_max_lines(Some(0))
                .expect("Terminal::new");
            terminal
        };
        t.vt_write(modes);
        t
    }

    /// Wire event at surface pixel (x, y). Cell-quantized clients emit
    /// `cell_index x cell_size` per SPEC input.md §3.1.
    fn event_at(action: MouseAction, button: MouseButton, x: f64, y: f64) -> MouseEvent {
        MouseEvent {
            action,
            button,
            mods: ModSet::empty(),
            x,
            y,
        }
    }

    /// phux-yyex regression: without `EncoderSize` libghostty encodes every
    /// mouse event to zero bytes, so the server silently dropped ALL mouse
    /// input. A click against an SGR-tracking terminal must produce bytes.
    #[test]
    fn click_encodes_sgr_bytes_with_cell_geometry() {
        // Claude Code's probed mode set: ?1000h ?1002h ?1003h ?1006h.
        let t = terminal_with(b"\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h");
        let mut enc = PerTerminalMouseEncoder::new().expect("encoder");
        // Cell 10,5 emitted at 8x16px cells => surface position (80, 80).
        let bytes = enc
            .encode(
                &event_at(MouseAction::Press, MouseButton::Left, 80.0, 80.0),
                &t,
                (8, 16),
            )
            .expect("encode");
        // SGR press, button 0, 1-based cell coords: col 11, row 6.
        assert_eq!(bytes, b"\x1b[<0;11;6M");
    }

    #[test]
    fn wheel_encodes_sgr_scroll_buttons() {
        let t = terminal_with(b"\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h");
        let mut enc = PerTerminalMouseEncoder::new().expect("encoder");
        let up = enc
            .encode(
                &event_at(MouseAction::Press, MouseButton::Four, 80.0, 80.0),
                &t,
                (8, 16),
            )
            .expect("encode")
            .to_vec();
        assert_eq!(up, b"\x1b[<64;11;6M");
        let down = enc
            .encode(
                &event_at(MouseAction::Press, MouseButton::Five, 80.0, 80.0),
                &t,
                (8, 16),
            )
            .expect("encode")
            .to_vec();
        assert_eq!(down, b"\x1b[<65;11;6M");
    }

    /// A terminal whose program never enabled mouse tracking still encodes
    /// to nothing — the drop is the ENCODER's mode decision, not a geometry
    /// accident.
    #[test]
    fn no_tracking_mode_encodes_to_nothing() {
        let t = terminal_with(b"");
        let mut enc = PerTerminalMouseEncoder::new().expect("encoder");
        let bytes = enc
            .encode(
                &event_at(MouseAction::Press, MouseButton::Left, 80.0, 80.0),
                &t,
                (8, 16),
            )
            .expect("encode");
        assert!(bytes.is_empty());
    }

    /// Degenerate zero cell geometry (a hostile or buggy resize) must not
    /// regress to the encode-to-nothing state: axes clamp to 1px.
    #[test]
    fn zero_cell_geometry_clamps_and_still_encodes() {
        let t = terminal_with(b"\x1b[?1000h\x1b[?1006h");
        let mut enc = PerTerminalMouseEncoder::new().expect("encoder");
        let bytes = enc
            .encode(
                &event_at(MouseAction::Press, MouseButton::Left, 10.0, 5.0),
                &t,
                (0, 0),
            )
            .expect("encode");
        // 1px cells: surface (10, 5) is cell (10, 5), 1-based (11, 6).
        assert_eq!(bytes, b"\x1b[<0;11;6M");
    }
}
