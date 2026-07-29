use libghostty_vt::{Terminal as GhosttyTerminal, render::Snapshot, terminal::Mode};

use super::synthesizer::{MOUSE_MODES, SynthesisError};

/// Per-consumer reference state for the ADR-0018 lazy state-sync diff,
/// owned by each attached consumer (phux-ia4).
///
/// Holds the last-synced rendered body of every viewport row plus the
/// last-synced cursor/mode state. [`super::SnapshotSynthesizer::synthesize_against_reference`]
/// diffs the live terminal against this and advances it on emit. It is
/// fully independent per consumer, so it does not depend on libghostty's
/// shared `Terminal` dirty bits (which `RenderState::update` consumes on
/// the first read each tick — the bug this whole type exists to fix).
#[derive(Debug, Clone, Default)]
pub struct ConsumerReference {
    /// Reference width. A geometry change resets the row bodies.
    pub(crate) cols: u16,
    /// Reference height.
    pub(crate) rows: u16,
    /// Per-row last-synced rendered cell body (one `Vec<u8>` per viewport
    /// row, indexed by zero-based row). Compared byte-for-byte against the
    /// freshly rendered row to decide whether the row changed.
    pub(crate) rows_body: Vec<Vec<u8>>,
    /// Last-synced cursor placement + DEC mode bits, diffed flat.
    pub(crate) cursor_mode: ReferenceCursorMode,
    /// Reusable scratch for the indices of rows that changed this tick,
    /// owned here (rather than freshly allocated per
    /// [`crate::grid::synthesizer::SnapshotSynthesizer::synthesize_against_reference`]
    /// call) so a
    /// steady stream of diffs reuses its capacity instead of allocating a
    /// fresh `Vec` each tick.
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

/// Cursor placement + the DEC mode bits the epilogue re-emits, captured
/// for the per-consumer reference diff (phux-ia4). Compared flat: any
/// field change triggers an epilogue re-emit.
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
    /// DEC mode 1049 (`ALT_SCREEN_SAVE`) — the one vim/less/man/htop use.
    /// Tracked alongside 47 so a 47<->1049 transition still trips the
    /// per-tick reference diff (it would be missed if only 47 were
    /// tracked, since the two are independent bits).
    pub(crate) alt_screen_save: bool,
    /// The mouse-reporting mode bits the epilogue re-emits: tracking level
    /// (9 / 1000 / 1002 / 1003), report encoding (1005 / 1006 / 1015 /
    /// 1016), and wheel policy (1007), in that order.
    ///
    /// Tracked so a program that flips mouse reporting *without* touching a
    /// row or the cursor (`:set mouse=` in vim, a TUI enabling the mouse for
    /// a modal) still forces an epilogue re-emit. Without it the consumer's
    /// mirror keeps the stale tracking state and mis-routes the wheel.
    pub(crate) mouse_modes: [bool; 9],
}

impl ReferenceCursorMode {
    /// Capture the live cursor/mode state. The `CursorVisualStyle` is not
    /// tracked here (it is re-emitted in the epilogue on every non-empty
    /// tick regardless); the fields captured are exactly those whose
    /// change can independently force an epilogue re-emit.
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

    /// The three alt-screen mode bits as a tuple, for detecting a screen
    /// transition independent of cursor/paste/focus changes. Used by the
    /// diff path to decide whether to re-toggle the screen buffer.
    pub(crate) const fn alt_screen_set(&self) -> (bool, bool, bool) {
        (
            self.alt_screen_legacy,
            self.alt_screen,
            self.alt_screen_save,
        )
    }
}
