//! What the user does to the shown terminal: keys and shortcuts, paste and
//! copy, history scrolling, and selection.

use phux_client_core::layout::{Direction, SplitDir};
use phux_client_runtime::ViewId;
use phux_client_runtime::control::{ControlPlane, DeliveryOutcome};
use phux_client_runtime::engine::ReplicaPrompt;
use phux_client_runtime::engine::Scroll;
use phux_client_runtime::publication::GridFrame;
use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::{Desk, NoticeKind, Pending};
use crate::keys::{self, Shortcut, Typed};

pub(super) mod selection;

/// What the window says while keys can't reach the terminal; the next key
/// that does takes it back.
const INPUT_PAUSED: &str = "Not connected; input isn't reaching the terminal.";

/// What has the keyboard instead of the terminal, in the order they claim it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modal {
    ContextMenu,
    Inbox,
    Machines,
    Settings,
    Rename,
    Palette,
    FindField,
    Confirm,
}

impl Modal {
    /// Whether a command, by chord or menu pick, may run while this has the
    /// keyboard. The find field leaves the shown terminal in view; a sheet
    /// hides what a command would touch.
    const fn allows(self, action: Shortcut) -> bool {
        match self {
            Self::FindField => true,
            Self::Palette => matches!(action, Shortcut::Palette),
            Self::Inbox | Self::Settings => action.works_over_sheets(),
            Self::ContextMenu | Self::Machines | Self::Rename | Self::Confirm => false,
        }
    }

    /// Whether this covers the panes as a Rune modal, which drops any held
    /// press without its release.
    pub(in crate::desk) const fn takes_pointer(self) -> bool {
        !matches!(self, Self::FindField | Self::Confirm)
    }
}

/// Whether a view's frame shows a highlight.
fn has_selection(frame: &GridFrame) -> bool {
    frame
        .buffer
        .cells
        .iter()
        .any(|cell| cell.flags & phux_client_core::grid::CELL_SELECTED != 0)
}

/// A paste that would arrive as typed keys with line breaks, each one an
/// Enter, or that carries the bracket terminator: the user confirms first,
/// as Ghostty's `clipboard-paste-protection` and the GPUI desktop do.
pub fn paste_needs_confirmation(bracketed: bool, text: &str) -> bool {
    text.contains("\x1b[201~") || (!bracketed && text.contains(['\n', '\r']))
}

impl Desk {
    /// What has the keyboard instead of the terminal, if anything: the one
    /// rule for keys, the input method and the IME's anchor in the grid.
    pub fn modal(&self) -> Option<Modal> {
        [
            (self.pointer.menu.is_some(), Modal::ContextMenu),
            (self.inbox.open, Modal::Inbox),
            (self.machines.open, Modal::Machines),
            (self.settings_open, Modal::Settings),
            (self.tabs.renaming.is_some(), Modal::Rename),
            (self.palette_open, Modal::Palette),
            (self.find_query.focused, Modal::FindField),
            (self.pending.is_some(), Modal::Confirm),
        ]
        .into_iter()
        .find_map(|(open, modal)| open.then_some(modal))
    }

    /// Every key nothing else took. True consumes it.
    pub fn key(&mut self, event: &rune::KeyEvent, tx: &mut Tx) -> bool {
        if let rune::Key::Modifier { .. } = event.key {
            self.modifiers_changed(event.mods.cmd);
            return false;
        }
        let action = keys::shortcut(&event.key, event.mods, tx.physical_key());
        if let Some(modal) = self.modal() {
            self.modal_key(modal, event, action, tx);
            return true;
        }
        if let Some(action) = action {
            self.shortcut(action, tx);
            return true;
        }
        self.type_key(event, tx)
    }

    /// A key while `modal` has the keyboard: never terminal input.
    fn modal_key(
        &mut self,
        modal: Modal,
        event: &rune::KeyEvent,
        action: Option<Shortcut>,
        tx: &mut Tx,
    ) {
        let escape = event.key == rune::Key::Escape;
        match modal {
            Modal::ContextMenu if escape => self.close_menu(),
            Modal::Rename if escape => self.cancel_rename(),
            Modal::Inbox if self.inbox_key(event, tx) => {}
            Modal::Machines => {
                self.machines_key(event, tx);
            }
            Modal::Settings if action.is_none() => self.settings_key(event),
            Modal::Confirm => self.confirm_key(event),
            _ => {
                if let Some(action) = action {
                    self.modal_command(modal, action, tx);
                }
            }
        }
    }

    /// A command while `modal` has the keyboard, from a chord or a menu pick.
    /// Whatever the field or sheet doesn't take is a command or nothing, never
    /// terminal input (Ctrl-C must not reach the job from the find field).
    pub(super) fn modal_command(&mut self, modal: Modal, action: Shortcut, tx: &mut Tx) {
        if !modal.allows(action) {
            return;
        }
        if modal == Modal::Palette {
            tx.blur();
        }
        self.shortcut(action, tx);
    }

    fn confirm_key(&mut self, event: &rune::KeyEvent) {
        match event.key {
            rune::Key::Enter => self.confirm_pending(),
            rune::Key::Escape => self.pending = None,
            _ => {}
        }
    }

    /// A key that is terminal input, or `false` when it isn't.
    fn type_key(&mut self, event: &rune::KeyEvent, tx: &mut Tx) -> bool {
        self.restart_blink();
        let Some(typed) = keys::typed(
            &event.key,
            event.mods,
            tx.key_base(),
            tx.physical_key(),
            self.settings.option_as_meta,
        ) else {
            return false;
        };
        self.send_input(|control, terminal| match typed {
            Typed::Text(text) => control.send_text(terminal, &text),
            Typed::Key(atom) => control.send_key(terminal, atom),
        });
        true
    }

    /// Send typed input to the focused terminal, whole or not at all: it
    /// returns to the live tail first and clears a stale highlight, as
    /// Ghostty does. Input the connection can't take now is said, not dropped
    /// quietly.
    fn send_input(&mut self, send: impl FnOnce(&mut ControlPlane, &ResourceId) -> bool) {
        let (Some(client), Some(shown)) = (self.client.clone(), &self.shown) else {
            return;
        };
        let (terminal, view) = (shown.terminal.clone(), shown.view);
        let frame = client.acquire_view(view);
        let behind = frame.as_ref().is_some_and(|f| !f.scrollbar.at_tail());
        let sent = client.with_control(|control| {
            if !control.input_ready(&terminal) {
                return false;
            }
            if behind {
                let _ = control.scroll_view(view, Scroll::Bottom);
            }
            send(control, &terminal)
        });
        if !sent {
            return self.say(NoticeKind::Error, INPUT_PAUSED);
        }
        if self.notice.as_ref().is_some_and(|n| n.text == INPUT_PAUSED) {
            self.notice = None;
        }
        if frame.is_some_and(|f| has_selection(&f))
            && let Some(engine) = client.engine()
        {
            let _ = engine.clear_view_selection(view);
        }
    }

    /// What the input method composed: dead keys, CJK, the emoji picker.
    /// Plain characters arrive as keys instead. While something else has the
    /// keyboard none of it is the terminal's.
    pub fn ime(&mut self, event: &rune::Ime) -> bool {
        if self.modal().is_some() {
            self.preedit = None;
            return false;
        }
        match event {
            rune::Ime::Preedit(text, _) => {
                self.preedit = Some(text.clone()).filter(|t| !t.is_empty());
            }
            rune::Ime::Commit(text) => {
                self.preedit = None;
                self.restart_blink();
                self.send_input(|control, terminal| control.send_text(terminal, text));
            }
            _ => return false,
        }
        true
    }

    pub fn shortcut(&mut self, action: Shortcut, tx: &mut Tx) {
        match action {
            Shortcut::Copy => self.copy(tx),
            Shortcut::SelectAll => self.select_all(),
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
            Shortcut::PromptUp => self.scroll(Scroll::Prompt(-1)),
            Shortcut::PromptDown => self.scroll(Scroll::Prompt(1)),
            Shortcut::Palette => self.toggle_palette(),
            Shortcut::ToggleSidebar => self.toggle_sidebar(),
            Shortcut::Inbox => self.toggle_inbox(),
            Shortcut::Settings => self.toggle_settings(),
            Shortcut::CheckForUpdates => self.updater.check_now(),
            Shortcut::InstallUpdate => self.install_update(tx),
            Shortcut::Machines => self.toggle_machines(),
            Shortcut::ToggleFullScreen => rune_mac::toggle_full_screen(),
            Shortcut::KeyboardShortcuts => self.show_shortcuts(),
            Shortcut::JumpToAttention => self.jump_to_attention(),
            Shortcut::SplitRight => self.split(SplitDir::Horizontal),
            Shortcut::SplitDown => self.split(SplitDir::Vertical),
            Shortcut::ZoomPane => self.toggle_zoom(),
            Shortcut::CloseTerminal => self.close_terminal(),
            Shortcut::CloseTab => self.close_active_tab(),
            Shortcut::NextTab => self.step_tab(1),
            Shortcut::PreviousTab => self.step_tab(-1),
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

    /// The user's clipboard paste. One that would type line breaks into a
    /// program without bracketed paste waits for confirmation.
    pub fn paste(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let (Some(client), Some(shown)) = (&self.client, &self.shown) else {
            return;
        };
        let bracketed = client
            .engine()
            .is_some_and(|engine| engine.bracketed_paste(&shown.terminal).unwrap_or(false));
        if paste_needs_confirmation(bracketed, text) {
            self.pending = Some(Pending::Paste(text.to_owned()));
            return;
        }
        self.send_paste(text);
    }

    pub fn confirm_pending(&mut self) {
        match self.pending.take() {
            Some(Pending::Paste(text)) => self.send_paste(&text),
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

    fn active_tab(&self) -> Option<usize> {
        self.tabs.list.iter().position(|tab| tab.active)
    }

    fn close_active_tab(&mut self) {
        if let Some(index) = self.active_tab() {
            self.close_tab(index);
        }
    }

    /// The tab `step` along from the active one, wrapping.
    fn step_tab(&mut self, step: isize) {
        let count = self.tabs.list.len() as isize;
        if let Some(index) = self.active_tab() {
            let next = (index as isize + step).rem_euclid(count) as usize;
            self.select_tab(next);
        }
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

    fn send_paste(&mut self, text: &str) {
        let Some(terminal) = self.shown.as_ref().map(|s| s.terminal.clone()) else {
            return;
        };
        if !self.deliver_paste(&terminal, text) {
            self.say(NoticeKind::Error, "Not connected; nothing was pasted.");
        }
    }

    /// Paste as the user, remembering the receipt so a refused or unknown
    /// delivery reaches them. False: the terminal can't take input now.
    pub(super) fn deliver_paste(&mut self, terminal: &ResourceId, text: &str) -> bool {
        let Some(client) = self.client.clone() else {
            return false;
        };
        if !client.input_ready(terminal) {
            return false;
        }
        // The server brackets it per DEC 2004 and classifies it. A paste too
        // large for acknowledged input goes as one atomic event, unreceipted.
        let receipt = client.with_control(|control| control.apply_user_paste(terminal, text));
        self.pastes.extend(receipt);
        client.wake();
        true
    }

    /// One acknowledged write resolved. Unknown is never retried: only the
    /// person can see whether it landed.
    pub(super) fn paste_delivered(&mut self, delivery: u64, outcome: DeliveryOutcome) -> bool {
        if !self.pastes.remove(&delivery) {
            return false;
        }
        match outcome {
            DeliveryOutcome::Delivered => return false,
            DeliveryOutcome::Refused => {
                self.say(NoticeKind::Error, "Paste refused; nothing was written.");
            }
            DeliveryOutcome::Unknown => self.say(
                NoticeKind::Error,
                "Paste may not have landed; look before pasting again.",
            ),
        }
        true
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
}

#[cfg(test)]
mod tests {
    use super::{Modal, Shortcut, paste_needs_confirmation};

    #[test]
    fn the_find_field_leaves_commands_running() {
        assert!(Modal::FindField.allows(Shortcut::FindNext));
        assert!(Modal::FindField.allows(Shortcut::FindPrevious));
    }

    #[test]
    fn sheets_stop_what_they_hide() {
        assert!(!Modal::Inbox.allows(Shortcut::CloseTerminal));
        assert!(Modal::Settings.allows(Shortcut::ZoomIn));
        assert!(!Modal::Confirm.allows(Shortcut::ZoomIn));
        assert!(!Modal::Palette.allows(Shortcut::FindNext));
        assert!(Modal::Palette.allows(Shortcut::Palette));
    }

    #[test]
    fn only_rune_modals_take_the_pointer() {
        assert!(Modal::ContextMenu.takes_pointer());
        assert!(Modal::Palette.takes_pointer());
        assert!(!Modal::FindField.takes_pointer());
        assert!(!Modal::Confirm.takes_pointer());
    }

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
