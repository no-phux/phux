//! Vi keys for copy-mode, following tmux's `copy-mode-vi` where there is a
//! choice: `h j k l`, `w b e`, `0 ^ $`, `g G`, `H M L`, `C-u C-d C-b C-f`,
//! `v V C-v` to select, `y` to copy.
//!
//! Word and line-end motions need the row's text, which only the dispatcher's
//! engine has: the overlay asks with a [`CopyMotionRequest`] and the
//! dispatcher answers with the target column from
//! [`phux_client_core::motion::row_motion_target`].

use phux_protocol::input::key::{KeyEvent, ModSet, PhysicalKey};

use phux_client_core::motion::RowMotion;

use super::CopyModeOverlay;
use crate::render::overlay::{OverlayCommand, ScreenSelectionPoint, SelectionGrab, SelectionMode};

/// A scroll far enough to reach either end of any scrollback.
const FAR: isize = i32::MAX as isize;

/// A [`RowMotion`] the overlay asks the dispatcher to resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyMotionRequest {
    /// The move to make.
    pub motion: RowMotion,
    /// Pane-local row the cursor is on.
    pub row: u16,
    /// Pane-local column the cursor is on.
    pub col: u16,
    /// The pane's width in cells.
    pub pane_cols: u16,
}

/// `h j k l` as the arrow keys they stand for; every other key unchanged.
pub(super) const fn arrow_alias(key: PhysicalKey) -> PhysicalKey {
    match key {
        PhysicalKey::H => PhysicalKey::ArrowLeft,
        PhysicalKey::J => PhysicalKey::ArrowDown,
        PhysicalKey::K => PhysicalKey::ArrowUp,
        PhysicalKey::L => PhysicalKey::ArrowRight,
        other => other,
    }
}

impl CopyModeOverlay {
    /// Whether anything beyond the cell under the cursor is selected.
    pub(super) const fn has_selection(&self) -> bool {
        self.selecting || self.anchor_row != self.cursor_row || self.anchor_col != self.cursor_col
    }

    /// Pin the selection's anchor to its terminal cell, so it stays put
    /// while the viewport scrolls under a keyboard selection. A no-op until
    /// the dispatcher has reported where the viewport is.
    pub(super) fn pin_anchor(&mut self) {
        if self.mouse_anchor_screen.is_some() {
            return;
        }
        if let Some(top) = self.screen_top {
            self.mouse_anchor_screen = Some(ScreenSelectionPoint {
                col: self.anchor_col,
                row: top.saturating_add(u32::from(self.anchor_row)),
            });
            self.mouse_anchor_viewport_row = Some(i32::from(self.anchor_row));
        }
    }

    /// Drop the selection: the anchor rejoins the cursor.
    pub(super) const fn collapse(&mut self) {
        self.anchor_row = self.cursor_row;
        self.anchor_col = self.cursor_col;
        self.mouse_anchor_screen = None;
        self.mouse_anchor_viewport_row = None;
    }

    /// The dispatcher's report of the full-screen row now on viewport row 0:
    /// a pinned anchor moves on screen by exactly what the viewport scrolled.
    pub(super) fn viewport_top_changed(&mut self, screen_top: u32) {
        self.screen_top = Some(screen_top);
        if let Some(anchor) = self.mouse_anchor_screen {
            let row = i64::from(anchor.row) - i64::from(screen_top);
            self.mouse_anchor_viewport_row =
                Some(i32::try_from(row).unwrap_or(if row < 0 { i32::MIN } else { i32::MAX }));
        }
    }

    /// `v`, `V`, `C-v`: start selecting in `mode` from the cursor, switch an
    /// open selection to `mode`, or (the same key again) drop it.
    fn toggle_select(&mut self, mode: SelectionMode) {
        if self.selecting && self.mode == mode {
            self.selecting = false;
            self.mode = SelectionMode::Char;
            self.collapse();
            return;
        }
        if !self.selecting {
            self.collapse();
            self.pin_anchor();
        }
        self.selecting = true;
        self.mode = mode;
    }

    const fn row_motion(&self, motion: RowMotion) -> OverlayCommand {
        OverlayCommand::Motion(CopyMotionRequest {
            motion,
            row: self.cursor_row,
            col: self.cursor_col,
            pane_cols: self.pane_cols,
        })
    }

    /// The vi keys that are not plain cursor steps; `None` leaves the key to
    /// the rest of copy-mode.
    pub(super) fn handle_vi_key(&mut self, key: &KeyEvent) -> Option<OverlayCommand> {
        let shift = key.mods.contains(ModSet::SHIFT);
        let bottom = self.pane_rows.saturating_sub(1);
        let half = (self.page_scroll_delta() / 2).max(1);
        if key.mods.contains(ModSet::CTRL) {
            return Some(match key.key {
                PhysicalKey::U => OverlayCommand::ScrollViewport(-half),
                PhysicalKey::D => OverlayCommand::ScrollViewport(half),
                PhysicalKey::B => OverlayCommand::ScrollViewport(-self.page_scroll_delta()),
                PhysicalKey::F => OverlayCommand::ScrollViewport(self.page_scroll_delta()),
                PhysicalKey::V => {
                    self.toggle_select(SelectionMode::Rect);
                    OverlayCommand::Stay
                }
                // A control chord is never a plain letter.
                _ => OverlayCommand::Stay,
            });
        }
        match key.text.as_deref() {
            Some("$") => return Some(self.row_motion(RowMotion::LineEnd)),
            Some("^") => return Some(self.row_motion(RowMotion::FirstNonBlank)),
            Some("0") => {
                self.place_cursor(self.cursor_row, 0, false);
                return Some(OverlayCommand::Stay);
            }
            _ => {}
        }
        Some(match key.key {
            PhysicalKey::W => self.row_motion(RowMotion::WordForward),
            PhysicalKey::B => self.row_motion(RowMotion::WordBack),
            PhysicalKey::E => self.row_motion(RowMotion::WordEnd),
            // `g` the oldest line, `G` the newest.
            PhysicalKey::G => {
                let (row, far) = if shift { (bottom, FAR) } else { (0, -FAR) };
                self.place_cursor(row, 0, false);
                OverlayCommand::ScrollViewport(far)
            }
            // `H` `M` `L`: the top, middle, and bottom row on screen.
            PhysicalKey::H | PhysicalKey::M | PhysicalKey::L if shift => {
                let row = match key.key {
                    PhysicalKey::H => 0,
                    PhysicalKey::M => bottom / 2,
                    _ => bottom,
                };
                self.place_cursor(row, self.cursor_col, false);
                OverlayCommand::Stay
            }
            PhysicalKey::V => {
                self.toggle_select(if shift {
                    SelectionMode::Line
                } else {
                    SelectionMode::Char
                });
                OverlayCommand::Stay
            }
            // `Y` copies the logical line; `y` the selection, or with none
            // open the word under the cursor.
            PhysicalKey::Y if shift => {
                OverlayCommand::Copy(self.copy_request_with(SelectionGrab::LineSemantic))
            }
            PhysicalKey::Y if self.has_selection() => OverlayCommand::Copy(self.copy_request()),
            PhysicalKey::Y => OverlayCommand::Copy(self.copy_request_with(SelectionGrab::Word)),
            PhysicalKey::Q => OverlayCommand::Dismiss,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use phux_protocol::input::key::KeyAction;

    use crate::render::overlay::RenderOverlay as _;

    fn press(overlay: &mut CopyModeOverlay, key: PhysicalKey, mods: ModSet) -> OverlayCommand {
        overlay.handle_key(&KeyEvent {
            action: KeyAction::Press,
            key,
            mods,
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        })
    }

    fn plain(overlay: &mut CopyModeOverlay, key: PhysicalKey) -> OverlayCommand {
        press(overlay, key, ModSet::empty())
    }

    #[test]
    fn hjkl_step_and_v_opens_a_selection_that_moves_extend() {
        let mut overlay = CopyModeOverlay::new(5, 5, 80, 24);
        plain(&mut overlay, PhysicalKey::H);
        plain(&mut overlay, PhysicalKey::K);
        assert_eq!((overlay.cursor_row, overlay.cursor_col), (4, 4));
        assert!(!overlay.has_selection(), "a plain step selects nothing");

        plain(&mut overlay, PhysicalKey::V);
        plain(&mut overlay, PhysicalKey::J);
        plain(&mut overlay, PhysicalKey::L);
        assert_eq!((overlay.anchor_row, overlay.anchor_col), (4, 4));
        assert_eq!((overlay.cursor_row, overlay.cursor_col), (5, 5));
        // A resolved word motion extends too.
        overlay.apply_copy_motion(9);
        assert_eq!((overlay.anchor_col, overlay.cursor_col), (4, 9));
        let OverlayCommand::Copy(req) = plain(&mut overlay, PhysicalKey::Y) else {
            panic!("y copies an open selection");
        };
        assert_eq!(req.grab, SelectionGrab::Rect);
        assert_eq!(
            (req.start_row, req.start_col, req.end_row, req.end_col),
            (4, 4, 5, 9)
        );

        // The same key again drops the selection; V and C-v pick the mode.
        plain(&mut overlay, PhysicalKey::V);
        assert!(!overlay.has_selection());
        press(&mut overlay, PhysicalKey::V, ModSet::SHIFT);
        assert_eq!(overlay.mode, SelectionMode::Line);
        press(&mut overlay, PhysicalKey::V, ModSet::CTRL);
        assert_eq!(overlay.mode, SelectionMode::Rect);
        assert!(overlay.selecting);
    }

    #[test]
    fn scroll_and_jump_keys_ask_for_the_right_scroll_or_motion() {
        let mut overlay = CopyModeOverlay::new(5, 5, 80, 24);
        let ctrl = |overlay: &mut CopyModeOverlay, key| press(overlay, key, ModSet::CTRL);
        assert_eq!(
            ctrl(&mut overlay, PhysicalKey::U),
            OverlayCommand::ScrollViewport(-11)
        );
        assert_eq!(
            ctrl(&mut overlay, PhysicalKey::D),
            OverlayCommand::ScrollViewport(11)
        );
        assert_eq!(
            ctrl(&mut overlay, PhysicalKey::B),
            OverlayCommand::ScrollViewport(-23)
        );
        assert_eq!(
            ctrl(&mut overlay, PhysicalKey::F),
            OverlayCommand::ScrollViewport(23)
        );
        // A control chord never acts as its plain letter.
        assert_eq!(ctrl(&mut overlay, PhysicalKey::Y), OverlayCommand::Stay);

        assert_eq!(
            plain(&mut overlay, PhysicalKey::G),
            OverlayCommand::ScrollViewport(-FAR)
        );
        assert_eq!((overlay.cursor_row, overlay.cursor_col), (0, 0));
        assert_eq!(
            press(&mut overlay, PhysicalKey::G, ModSet::SHIFT),
            OverlayCommand::ScrollViewport(FAR)
        );
        assert_eq!(overlay.cursor_row, 23);
        press(&mut overlay, PhysicalKey::M, ModSet::SHIFT);
        assert_eq!(overlay.cursor_row, 11);

        let OverlayCommand::Motion(req) = plain(&mut overlay, PhysicalKey::W) else {
            panic!("w asks the dispatcher for the next word");
        };
        assert_eq!(
            (req.motion, req.row, req.col),
            (RowMotion::WordForward, 11, 0)
        );
        assert_eq!(plain(&mut overlay, PhysicalKey::Q), OverlayCommand::Dismiss);
    }

    /// `v`, then scrolling up two pages: the anchor stays on its terminal
    /// cell, the highlight runs to the viewport's edge, and the copy request
    /// names the anchor in full-screen rows.
    #[test]
    fn a_selection_keeps_its_anchor_cell_while_the_viewport_scrolls() {
        let mut overlay = CopyModeOverlay::new(3, 7, 80, 24);
        overlay.set_copy_viewport_top(100);
        plain(&mut overlay, PhysicalKey::V);
        overlay.set_copy_viewport_top(60);
        plain(&mut overlay, PhysicalKey::K);

        let shown = overlay.copy_selection().expect("copy-mode has a selection");
        assert_eq!((shown.start_row, shown.start_col), (2, 7));
        assert_eq!((shown.end_row, shown.end_col), (23, 79));
        let OverlayCommand::Copy(req) = plain(&mut overlay, PhysicalKey::Y) else {
            panic!("y copies");
        };
        assert_eq!(
            req.mouse_anchor_screen,
            Some(ScreenSelectionPoint { col: 7, row: 103 })
        );
        assert_eq!((req.cursor_row, req.cursor_col), (2, 7));
    }
}
