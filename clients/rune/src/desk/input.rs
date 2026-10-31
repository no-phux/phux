//! What the user does to the shown terminal: keys and shortcuts, paste and
//! copy, history scrolling, and selection.

use phux_client_core::layout::{Direction, SplitDir};
use phux_client_runtime::ViewId;
use phux_client_runtime::engine::ReplicaPrompt;
use phux_client_runtime::engine::{Scroll, SelectionGestureEvent};
use phux_protocol::ids::ResourceId;
use rune::{Phase, Tx};

use super::{Desk, NoticeKind, Pending};
use crate::keys::{self, Shortcut, Typed};

/// A paste that would arrive as typed keys with line breaks, each one an
/// Enter, or that carries the bracket terminator: the user confirms first,
/// as Ghostty's `clipboard-paste-protection` and the GPUI desktop do.
pub fn paste_needs_confirmation(bracketed: bool, text: &str) -> bool {
    text.contains("\x1b[201~") || (!bracketed && text.contains(['\n', '\r']))
}

impl Desk {
    /// Every key nothing else took. True consumes it.
    pub fn key(&mut self, event: &rune::KeyEvent, tx: &mut Tx) -> bool {
        if let rune::Key::Modifier { .. } = event.key {
            self.modifiers_changed(event.mods.cmd);
            return false;
        }
        if self.inbox.open {
            let physical = tx.physical_key();
            if !self.inbox_key(event, tx)
                && let Some(action) = keys::shortcut(&event.key, event.mods, physical)
                    .filter(|a| a.works_over_sheets())
            {
                self.shortcut(action, tx);
            }
            return true;
        }
        if self.machines.open {
            self.machines_key(event, tx);
            return true;
        }
        if self.settings_open {
            let physical = tx.physical_key();
            match keys::shortcut(&event.key, event.mods, physical) {
                Some(action) if action.works_over_sheets() => self.shortcut(action, tx),
                Some(_) => {}
                None => self.settings_key(event),
            }
            return true;
        }
        if self.tabs.renaming.is_some() {
            if event.key == rune::Key::Escape {
                self.cancel_rename();
            }
            return true;
        }
        if self.palette_open {
            // The field has the keys; only the chord that closes it gets here.
            let physical = tx.physical_key();
            if keys::shortcut(&event.key, event.mods, physical) == Some(Shortcut::Palette) {
                self.close_palette();
                tx.blur();
            }
            return true;
        }
        if self.find_query.focused {
            // The field has the keys; what it leaves is a shortcut or
            // nothing, never terminal input (Ctrl-C must not reach the job).
            let physical = tx.physical_key();
            if let Some(action) = keys::shortcut(&event.key, event.mods, physical) {
                self.shortcut(action, tx);
            }
            return true;
        }
        if self.pending.is_some() {
            match event.key {
                rune::Key::Enter => self.confirm_pending(),
                rune::Key::Escape => self.pending = None,
                _ => {}
            }
            return true;
        }
        let physical = tx.physical_key();
        if let Some(action) = keys::shortcut(&event.key, event.mods, physical)
            .or_else(|| keys::scroll_shortcut(&event.key, event.mods))
        {
            self.shortcut(action, tx);
            return true;
        }
        self.restart_blink();
        let Some(typed) = keys::typed(
            &event.key,
            event.mods,
            tx.key_base(),
            physical,
            self.settings.option_as_meta,
        ) else {
            return false;
        };
        let (Some(client), Some(shown)) = (&self.client, &self.shown) else {
            return true;
        };
        let terminal = shown.terminal.clone();
        let view = shown.view;
        client.with_control(|control| match typed {
            Typed::Text(text) => control.send_text(&terminal, &text),
            Typed::Key(atom) => control.send_key(&terminal, atom),
        });
        // Typing returns to the live tail and clears a stale highlight, as
        // Ghostty does; neither republishes when there is nothing to undo.
        let Some(frame) = client.acquire_view(view) else {
            return true;
        };
        let selected = frame
            .buffer
            .cells
            .iter()
            .any(|cell| cell.flags & phux_client_core::grid::CELL_SELECTED != 0);
        if selected && let Some(engine) = client.engine() {
            let _ = engine.clear_view_selection(view);
        }
        if !frame.scrollbar.at_tail() {
            let _ = client.with_control(|control| control.scroll_view(view, Scroll::Bottom));
        }
        true
    }

    /// What the input method composed: dead keys, CJK, the emoji picker.
    /// Plain characters arrive as keys instead.
    pub fn ime(&mut self, event: &rune::Ime) -> bool {
        match event {
            rune::Ime::Preedit(text, _) => {
                self.preedit = Some(text.clone()).filter(|t| !t.is_empty());
            }
            rune::Ime::Commit(text) => {
                self.preedit = None;
                if let (Some(client), Some(shown)) = (&self.client, &self.shown) {
                    client.with_control(|control| control.send_text(&shown.terminal, text));
                }
            }
            _ => return false,
        }
        true
    }

    pub fn shortcut(&mut self, action: Shortcut, tx: &mut Tx) {
        match action {
            Shortcut::Copy => self.copy(tx),
            Shortcut::Paste => tx.read_clipboard(|desk: &mut Desk, content, _| {
                desk.paste_content(content);
            }),
            Shortcut::NewTerminal => self.new_tab(),
            Shortcut::Tab(index) => self.select_tab(usize::from(index)),
            Shortcut::RenameSession => self.rename_selected_session(),
            Shortcut::RenameTab => self.rename_active_tab(),
            Shortcut::CloseSession => self.close_session(),
            Shortcut::NewSession => self.new_session(),
            Shortcut::NextTerminal => self.cycle(1),
            Shortcut::PreviousTerminal => self.cycle(-1),
            Shortcut::ZoomIn => self.zoom(Some(self.font_size + 1.0)),
            Shortcut::ZoomOut => self.zoom(Some(self.font_size - 1.0)),
            Shortcut::ZoomReset => self.zoom(None),
            Shortcut::ScrollTop => self.scroll(Scroll::Top),
            Shortcut::ScrollBottom => self.scroll(Scroll::Bottom),
            Shortcut::Palette => self.toggle_palette(),
            Shortcut::ToggleSidebar => self.toggle_sidebar(),
            Shortcut::Inbox => self.toggle_inbox(),
            Shortcut::Settings => self.toggle_settings(),
            Shortcut::CheckForUpdates => self.updater.check_now(),
            Shortcut::InstallUpdate => self.install_update(tx),
            Shortcut::Machines => self.toggle_machines(),
            Shortcut::JumpToAttention => self.jump_to_attention(),
            Shortcut::SplitRight => self.split(SplitDir::Horizontal),
            Shortcut::SplitDown => self.split(SplitDir::Vertical),
            Shortcut::ZoomPane => self.toggle_zoom(),
            Shortcut::CloseTerminal => self.close_terminal(),
            Shortcut::FocusLeft => self.focus_toward(Direction::Left),
            Shortcut::FocusRight => self.focus_toward(Direction::Right),
            Shortcut::FocusUp => self.focus_toward(Direction::Up),
            Shortcut::FocusDown => self.focus_toward(Direction::Down),
            Shortcut::Find => self.open_find(),
            Shortcut::FindNext => self.step_find(-1),
            Shortcut::FindPrevious => self.step_find(1),
            Shortcut::FindSelection => self.find_selection(),
        }
    }

    pub(super) fn copy(&self, tx: &mut Tx) {
        let (Some(client), Some(shown)) = (&self.client, &self.shown) else {
            return;
        };
        let Some(engine) = client.engine() else {
            return;
        };
        if let Ok(bytes) = engine.view_selection_text(shown.view)
            && !bytes.is_empty()
        {
            tx.copy(String::from_utf8_lossy(&bytes).into_owned());
        }
    }

    /// The user's clipboard paste. One that would type line breaks into a
    /// program without bracketed paste waits for confirmation.
    pub fn paste(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let (Some(client), Some(shown)) = (&self.client, &self.shown) else {
            return;
        };
        let terminal = shown.terminal.clone();
        let bracketed = client
            .engine()
            .is_some_and(|engine| engine.bracketed_paste(&terminal).unwrap_or(false));
        if paste_needs_confirmation(bracketed, text) {
            self.pending = Some(Pending::Paste(terminal, text.to_owned()));
            return;
        }
        self.send_paste(&terminal, text);
    }

    pub fn confirm_pending(&mut self) {
        match self.pending.take() {
            Some(Pending::Paste(terminal, text)) => self.send_paste(&terminal, &text),
            Some(Pending::Close(terminal)) => self.close_now(&terminal),
            Some(Pending::CloseTab(terminals)) => {
                if let Some(client) = &self.client {
                    let _ = client.close_terminals(terminals);
                }
            }
            Some(Pending::CloseSession(key, _)) => {
                if let Some(client) = &self.client
                    && let Err(error) = client.kill_session(key)
                {
                    self.say(NoticeKind::Error, format!("Session not closed: {error}"));
                }
            }
            None => {}
        }
    }

    pub fn cancel_pending(&mut self) {
        self.pending = None;
    }

    /// Close the focused terminal: at once at a prompt shell integration
    /// vouches for, after asking otherwise. Closing is not undoable, so
    /// "unknown" asks too.
    pub fn close_terminal(&mut self) {
        let (Some(client), Some(shown)) = (&self.client, &self.shown) else {
            return;
        };
        let terminal = shown.terminal.clone();
        if !self.settings.confirm_close {
            self.close_now(&terminal);
            return;
        }
        let idle = match client.with_control(|control| control.command_state(&terminal)) {
            Some(running) => !running,
            None => client
                .engine()
                .is_some_and(|engine| engine.replica_prompt(&terminal) == ReplicaPrompt::AtPrompt),
        };
        if idle {
            self.close_now(&terminal);
        } else {
            self.pending = Some(Pending::Close(terminal));
        }
    }

    fn close_now(&mut self, terminal: &ResourceId) {
        if let Some(client) = &self.client {
            let _ = client.close_terminals(vec![terminal.clone()]);
        }
    }

    /// To the terminal it was meant for, wherever the keyboard went since.
    fn send_paste(&mut self, terminal: &ResourceId, text: &str) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if !self.lists(terminal) {
            self.say(
                NoticeKind::Error,
                "That terminal closed; nothing was pasted.",
            );
            return;
        }
        // The server brackets it per DEC 2004 and classifies it. A paste too
        // large for acknowledged input goes as one atomic event, unreceipted.
        client.with_control(|control| control.apply_user_paste(terminal, text));
        client.wake();
    }

    fn scroll(&mut self, scroll: Scroll) {
        let (Some(client), Some(shown)) = (&self.client, &self.shown) else {
            return;
        };
        let view = shown.view;
        let _ = client.with_control(|control| control.scroll_view(view, scroll));
        self.generation += 1;
    }

    /// Back to the live tail of a pane scrolled into history.
    pub fn follow_live(&mut self, terminal: &ResourceId) {
        let (Some(client), Some(view)) = (self.client.clone(), self.view_of(terminal)) else {
            return;
        };
        let _ = client.with_control(|control| control.scroll_view(view, Scroll::Bottom));
        self.generation += 1;
    }

    /// The view showing `terminal`, focused or not.
    pub(super) fn view_of(&self, terminal: &ResourceId) -> Option<ViewId> {
        match &self.shown {
            Some(shown) if &shown.terminal == terminal => Some(shown.view),
            _ => self.others.get(terminal).copied(),
        }
    }

    /// A press, drag, or release over the grid selects through the engine.
    pub fn select(&mut self, terminal: &ResourceId, phase: Phase, x: f32, y: f32, clicks: u32) {
        let Some(view) = self.view_of(terminal) else {
            return;
        };
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(frame) = client.acquire_view(view) else {
            return;
        };
        let Some(engine) = client.engine() else {
            return;
        };
        let metrics = self.metrics();
        let (col, row) = metrics.cell_at(x, y, frame.cols, frame.rows);
        let (cell_w, cell_h) = metrics.cell_px();
        let phase = match phase {
            Phase::Down => 0,
            Phase::Move => 1,
            Phase::Up => 2,
        };
        let event = SelectionGestureEvent {
            phase,
            clicks: clicks.clamp(1, 3),
            handle: self.gesture,
            column: col,
            rectangle: false,
            row: u32::from(row),
            x: f64::from(x * metrics.scale),
            y: f64::from(y * metrics.scale),
            columns: u32::from(frame.cols),
            cell_width: u32::from(cell_w),
            screen_height: (u32::from(frame.rows) * u32::from(cell_h)).max(1),
            padding_left: 0,
        };
        if let Ok(result) = engine.view_selection_gesture(view, event) {
            self.gesture = if phase == 2 { 0 } else { result.handle };
        }
        self.generation += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::paste_needs_confirmation;

    #[test]
    fn line_breaks_ask_only_without_bracketed_paste() {
        assert!(paste_needs_confirmation(false, "ls\nrm -rf x\n"));
        assert!(paste_needs_confirmation(false, "a\rb"));
        assert!(!paste_needs_confirmation(true, "ls\nrm -rf x\n"));
        assert!(!paste_needs_confirmation(false, "one line"));
    }

    #[test]
    fn the_bracket_terminator_always_asks() {
        assert!(paste_needs_confirmation(true, "x\x1b[201~y"));
    }
}
