use libghostty_vt::{Terminal as GhosttyTerminal, render::Snapshot, terminal::Mode};

use super::synthesizer::{MOUSE_MODES, SynthesisError};

/// Per-consumer reference for the state-sync diff (ADR-0018): last-synced
/// row bodies and cursor/mode state, advanced on emit and independent of
/// libghostty's shared dirty bits.
#[derive(Debug, Clone, Default)]
pub struct ConsumerReference {
    /// Reference width. A geometry change resets the row bodies.
    pub(crate) cols: u16,
    /// Reference height.
    pub(crate) rows: u16,
    /// Last-synced rendered body per viewport row.
    pub(crate) rows_body: Vec<Vec<u8>>,
    /// Last-synced cursor placement + DEC mode bits, diffed flat.
    pub(crate) cursor_mode: ReferenceCursorMode,
    /// Reused scratch for this tick's changed row indices.
    pub(crate) changed_scratch: Vec<u16>,
}

impl ConsumerReference {
    /// A fresh, empty reference. The first `prime_reference` /
    /// `synthesize_against_reference` sizes it to the live geometry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Resize the reference to `cols x rows`, clearing every row body so
    /// the next diff treats all rows as changed (full repaint).
    pub(crate) fn reset_geometry(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.rows_body = vec![Vec::new(); usize::from(rows)];
        self.cursor_mode = ReferenceCursorMode::default();
    }
}

/// Cursor and epilogue mode bits for the reference diff; any change
/// re-emits the epilogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "DEC mode bits are independent flags; a bitfield would obscure the per-flag mapping"
)]
pub(crate) struct ReferenceCursorMode {
    pub(crate) cursor_x: Option<u16>,
    pub(crate) cursor_y: Option<u16>,
    pub(crate) cursor_visible: bool,
    pub(crate) cursor_blinking: bool,
    pub(crate) bracketed_paste: bool,
    pub(crate) focus_event: bool,
    /// DEC mode 47 (`ALT_SCREEN_LEGACY`).
    pub(crate) alt_screen_legacy: bool,
    /// DEC mode 1047 (`ALT_SCREEN`).
    pub(crate) alt_screen: bool,
    /// DEC 1049; tracked with 47 since they are independent bits.
    pub(crate) alt_screen_save: bool,
    /// Mouse mode bits (tracking, encoding, 1007), so a mode flip with no row
    /// change still re-emits the epilogue.
    pub(crate) mouse_modes: [bool; 9],
}

impl ReferenceCursorMode {
    /// Capture the fields whose change forces an epilogue re-emit.
    pub(crate) fn capture(
        snapshot: &Snapshot<'_, '_>,
        terminal: &GhosttyTerminal<'_, '_>,
    ) -> Result<Self, SynthesisError> {
        let (cursor_x, cursor_y) = snapshot
            .cursor_viewport()?
            .map_or((None, None), |v| (Some(v.x), Some(v.y)));
        Ok(Self {
            cursor_x,
            cursor_y,
            cursor_visible: snapshot.cursor_visible()?,
            cursor_blinking: snapshot.cursor_blinking()?,
            bracketed_paste: terminal.mode(Mode::BRACKETED_PASTE).unwrap_or(false),
            focus_event: terminal.mode(Mode::FOCUS_EVENT).unwrap_or(false),
            alt_screen_legacy: terminal.mode(Mode::ALT_SCREEN_LEGACY).unwrap_or(false),
            alt_screen: terminal.mode(Mode::ALT_SCREEN).unwrap_or(false),
            alt_screen_save: terminal.mode(Mode::ALT_SCREEN_SAVE).unwrap_or(false),
            mouse_modes: MOUSE_MODES.map(|(m, _)| terminal.mode(m).unwrap_or(false)),
        })
    }

    /// The three alt-screen bits, to detect a screen transition.
    pub(crate) const fn alt_screen_set(&self) -> (bool, bool, bool) {
        (
            self.alt_screen_legacy,
            self.alt_screen,
            self.alt_screen_save,
        )
    }
}
