//! Opening, moving through, and running the command palette.

use rune::{Key, KeyEvent, Tx};

use super::Desk;
use crate::palette::{self, Hit, Target};

impl Desk {
    pub fn toggle_palette(&mut self) {
        if self.palette_open {
            self.close_palette();
            return;
        }
        self.palette_open = true;
        self.palette_picked = 0;
        self.palette_query.clear();
        // Ask once; the runtime keeps the field's focus from here.
        self.palette_query.focused = true;
    }

    pub fn close_palette(&mut self) {
        self.palette_open = false;
        self.palette_query.clear();
        self.palette_query.focused = false;
    }

    /// The palette's rows for the current query, best first.
    pub fn palette_hits(&self) -> Vec<Hit> {
        palette::rank(
            palette::items(self.topology.as_ref()),
            &self.palette_query.text,
        )
    }

    /// Arrow keys and Escape inside the palette's field.
    pub fn palette_key(&mut self, event: &KeyEvent, tx: &mut Tx) -> bool {
        let count = self.palette_hits().len();
        match event.key {
            Key::Down | Key::Tab if count > 0 => {
                self.palette_picked = (self.palette_picked + 1) % count
            }
            Key::Up if count > 0 => self.palette_picked = (self.palette_picked + count - 1) % count,
            Key::Escape => {
                self.close_palette();
                tx.blur();
            }
            _ => return false,
        }
        true
    }

    /// Run the highlighted row, or the row at `index`.
    pub fn palette_run(&mut self, index: Option<usize>, tx: &mut Tx) {
        let hits = self.palette_hits();
        let Some(hit) = hits.get(index.unwrap_or(self.palette_picked)).cloned() else {
            return;
        };
        self.close_palette();
        tx.blur();
        match hit.item.target {
            Target::Terminal { id, session } => self.show(id, &session),
            Target::Session(name) => self.show_session(&name),
            Target::Action(action) => self.shortcut(action, tx),
        }
    }

    /// Select a session and show its focused terminal.
    pub fn show_session(&mut self, name: &str) {
        let Some(topology) = &self.topology else {
            return;
        };
        let Some(session) = topology.sessions.iter().find(|s| s.name == name) else {
            return;
        };
        let panes = || topology.panes.iter().filter(|p| p.session_id == session.id);
        let Some(pane) = panes()
            .find(|p| p.terminal_id == topology.focused_pane)
            .or_else(|| panes().next())
        else {
            return;
        };
        let (id, session) = (pane.terminal_id.clone(), pane.session_name.clone());
        self.show(id, &session);
    }
}
