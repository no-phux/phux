//! Input from a pane: which window it means, and getting it to Neovim.

use super::{Daemon, Surface};
use crate::input::{Event as Input, Mouse};

impl Daemon {
    /// Input in a pane. Typing in a pane makes its window current, with one
    /// exception: phux's focus is the client's and a plugin can't move it,
    /// so after `<C-w>l` the keys keep arriving from the pane that sent it.
    /// Those follow Neovim to its current window. A click always selects.
    pub(super) fn on_input(&mut self, id: u32, bytes: &[u8]) {
        let Some(s) = self.pane_mut(id).and_then(|p| p.surface.as_mut()) else {
            return;
        };
        let events = s.parser.feed(bytes);
        if events.is_empty() {
            return;
        }
        let win = self.pane(id).and_then(|p| p.win);
        let clicked = events
            .iter()
            .any(|e| matches!(e, Input::Mouse(m) if m.is_press()));
        if let Some(win) = win
            && win != self.current
            && (clicked || self.typing != Some(id))
        {
            self.current = win;
            self.current_req = Some(self.nvim.request("nvim_set_current_win", vec![win.into()]));
            self.follow_focus();
        }
        self.typing = Some(id);
        // `nvim_input` is handled on arrival, ahead of a window change still
        // waiting for a safe point, so keys wait for that change's answer.
        if self.current_req.is_some() {
            self.held.push((id, events));
            self.nvim.flush();
        } else {
            self.send_input(id, events);
        }
    }

    pub(super) fn release_held(&mut self) {
        for (id, events) in std::mem::take(&mut self.held) {
            self.send_input(id, events);
        }
    }

    /// Keys are Neovim's wherever they came from; a mouse event means the
    /// pane it landed on.
    fn send_input(&mut self, id: u32, events: Vec<Input>) {
        let win = self.pane(id).and_then(|p| p.win);
        let grid = win.and_then(|w| self.grids.grid_of(w)).unwrap_or(0);
        for event in events {
            match event {
                Input::Key(k) => {
                    self.nvim.request("nvim_input", vec![k.into()]);
                }
                Input::Paste(text, phase) => {
                    self.nvim
                        .request("nvim_paste", vec![text.into(), true.into(), phase.into()]);
                }
                Input::Mouse(m) => self.send_mouse(id, grid, m),
            }
        }
        self.nvim.flush();
    }

    fn send_mouse(&mut self, id: u32, grid: u32, m: Mouse) {
        let Some(s) = self.pane_mut(id).and_then(|p| p.surface.as_mut()) else {
            return;
        };
        let (grid, row, col) = route(s, grid, &m);
        self.nvim.request(
            "nvim_input_mouse",
            vec![
                m.button.into(),
                m.action.into(),
                m.mods.into(),
                grid.into(),
                row.into(),
                col.into(),
            ],
        );
    }
}

/// The grid a mouse event lands on, and where on it: the topmost float
/// under it, or the float its button went down on.
fn route(s: &mut Surface, grid: u32, m: &Mouse) -> (u32, i32, i32) {
    let held = matches!(m.action, "drag" | "release");
    let hit = if held && s.grab.is_some() {
        s.grab
    } else {
        s.view
            .rects
            .iter()
            .rev()
            .find(|r| r.contains(m.row, m.col))
            .copied()
    };
    if m.is_press() && m.button != "wheel" {
        s.grab = hit;
    }
    if m.action == "release" {
        s.grab = None;
    }
    hit.map_or((grid, m.row, m.col), |t| {
        (t.grid, (m.row - t.row).max(0), (m.col - t.col).max(0))
    })
}
