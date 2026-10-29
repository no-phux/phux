//! ⌘-click (Ctrl-click elsewhere) opens a link; holding ⌘ over one
//! underlines it and shows where it goes.

use phux_protocol::ids::ResourceId;
use rune::{Phase, Tx};

use super::Desk;
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
}

impl Desk {
    fn link_at(&self, terminal: &ResourceId, x: f32, y: f32) -> Option<Link> {
        let frame = self
            .client
            .as_ref()?
            .acquire_view(self.view_of(terminal)?)?;
        let (col, row) = self.metrics().cell_at(x, y, frame.cols, frame.rows);
        links::at(&frame, col, row)
    }

    fn set_hover(&mut self, hover: Option<(ResourceId, Link)>) {
        if self.pointer.hover != hover {
            self.pointer.hover = hover;
            self.generation += 1;
        }
    }

    /// The pointer moved over a pane, `x, y` in its grid's points.
    pub fn hover(&mut self, terminal: &ResourceId, x: f32, y: f32, cmd: bool) {
        self.pointer.last = Some((terminal.clone(), x, y));
        let link = cmd.then(|| self.link_at(terminal, x, y)).flatten();
        self.set_hover(link.map(|link| (terminal.clone(), link)));
    }

    /// ⌘ went down or up without the pointer moving.
    pub fn modifiers_changed(&mut self, cmd: bool) {
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
        let Some(link) = cmd.then(|| self.link_at(terminal, x, y)).flatten() else {
            return false;
        };
        if let Err(error) = tx.open_url(&link.url) {
            self.notice = Some(format!("Couldn't open {}: {error:?}", link.url));
        }
        self.pointer.opened = true;
        true
    }
}
