//! ⌘-click (Ctrl-click elsewhere) opens a link; holding ⌘ over one
//! underlines it and shows where it goes.

use phux_protocol::ids::ResourceId;
use rune::{Mods, Phase, PointerPress, Tx};

use super::input::selection::Held;
use super::{Desk, NoticeKind};
use crate::links::{self, Link};

#[derive(Default)]
pub struct Pointer {
    /// The link ⌘ is over, and its pane.
    pub hover: Option<(ResourceId, Link)>,
    /// Where the pointer last moved over a pane, for a ⌘ press that comes
    /// without motion.
    last: Option<(ResourceId, f32, f32)>,
    /// A ⌘-press opened a link; its drag and release go nowhere else.
    opened: bool,
    /// ⌘ as the last pointer or modifier event left it.
    cmd: bool,
    /// A selection drag autoscrolling past the grid.
    pub(super) held: Option<Held>,
    /// The context menu, while open, at its window point.
    pub menu: Option<PointerPress>,
    /// The window's size in points, as the open menu last measured it.
    pub window: [f32; 2],
}

impl Desk {
    fn link_at(&self, terminal: &ResourceId, x: f32, y: f32) -> Option<Link> {
        let frame = self
            .client
            .as_ref()?
            .acquire_view(self.view_of(terminal)?)?;
        let m = self.metrics();
        // The padding, rail and pill are not the edge cell.
        let inside = x >= 0.0
            && y >= 0.0
            && x < f32::from(frame.cols) * m.cell_w
            && y < f32::from(frame.rows) * m.cell_h;
        if !inside {
            return None;
        }
        let (col, row) = m.cell_at(x, y, frame.cols, frame.rows);
        links::at(&frame, col, row)
    }

    fn set_hover(&mut self, hover: Option<(ResourceId, Link)>) {
        if self.pointer.hover != hover {
            self.pointer.hover = hover;
            self.generation += 1;
        }
    }

    /// The pointer moved over a pane, `x, y` in its grid's points.
    pub fn hover(&mut self, terminal: &ResourceId, x: f32, y: f32, mods: Mods) {
        let cmd = mods.cmd;
        self.pointer.last = Some((terminal.clone(), x, y));
        self.pointer.cmd = cmd;
        self.report_motion(terminal, x, y, mods);
        let link = cmd.then(|| self.link_at(terminal, x, y)).flatten();
        self.set_hover(link.map(|link| (terminal.clone(), link)));
    }

    /// The pointer left a pane: nothing there is under it any more.
    pub fn pointer_left(&mut self, terminal: &ResourceId) {
        self.forget_mouse_cell();
        if self
            .pointer
            .last
            .as_ref()
            .is_some_and(|(t, _, _)| t == terminal)
        {
            self.pointer.last = None;
            self.set_hover(None);
        }
    }

    /// A pane's output moved under a held ⌘: find the link again.
    pub(super) fn relink(&mut self, terminal: &ResourceId) {
        if self
            .pointer
            .last
            .as_ref()
            .is_some_and(|(t, _, _)| t == terminal)
        {
            self.modifiers_changed(self.pointer.cmd);
        }
    }

    /// ⌘ went down or up without the pointer moving.
    pub fn modifiers_changed(&mut self, cmd: bool) {
        self.pointer.cmd = cmd;
        let link = match (&self.pointer.last, cmd) {
            (Some((terminal, x, y)), true) => self
                .link_at(terminal, *x, *y)
                .map(|link| (terminal.clone(), link)),
            _ => None,
        };
        self.set_hover(link);
    }

    /// A ⌘-press on a link opens it. True: this gesture is the link's.
    pub fn press_link(
        &mut self,
        terminal: &ResourceId,
        phase: Phase,
        x: f32,
        y: f32,
        cmd: bool,
        tx: &mut Tx,
    ) -> bool {
        if phase != Phase::Down {
            let opened = self.pointer.opened;
            if phase == Phase::Up {
                self.pointer.opened = false;
            }
            return opened;
        }
        // A new press; a release lost to another app ends the old one.
        self.pointer.opened = false;
        let Some(link) = cmd.then(|| self.link_at(terminal, x, y)).flatten() else {
            return false;
        };
        if let Err(error) = tx.open_url(&link.url) {
            self.say(
                NoticeKind::Error,
                format!("Couldn't open {}: {error:?}", link.url),
            );
        }
        self.pointer.opened = true;
        true
    }
}
