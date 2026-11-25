//! What the user does to the shown terminal: keys and shortcuts, paste and
//! copy, history scrolling, and selection.

use phux_client_runtime::ViewId;
use phux_client_runtime::control::{ControlPlane, DeliveryOutcome};
use phux_client_runtime::engine::Scroll;
use phux_client_runtime::publication::GridFrame;
use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::{Desk, NoticeKind, Pending};
use crate::keys::{self, Shortcut, Typed};

mod echo;
pub(super) mod focus;
mod modal;
pub(super) mod selection;
mod shortcuts;
pub use modal::Modal;

/// What the window says while keys can't reach the terminal; the next key
/// that does takes it back.
pub(super) const INPUT_PAUSED: &str = "Not connected; input isn't reaching the terminal.";

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
/// as Ghostty's `clipboard-paste-protection` does.
pub fn paste_needs_confirmation(bracketed: bool, text: &str) -> bool {
    text.contains("\x1b[201~") || (!bracketed && text.contains(['\n', '\r']))
}

impl Desk {
    /// Recheck queued clipboard work against today's foreground owner.
    /// A paste must never replace a held question or address a hidden pane.
    pub(super) fn pastes_allowed(&self) -> bool {
        self.modal()
            .is_none_or(|modal| modal.allows(Shortcut::Paste))
    }

    /// What has the keyboard instead of the terminal, if anything: the one
    /// rule for keys, the input method and the IME's anchor in the grid.
    pub fn modal(&self) -> Option<Modal> {
        self.foreground().or_else(|| {
            [
                // A question outranks the field: the bar says what its keys do.
                (self.pending.is_some(), Modal::Confirm),
                (
                    self.phone.as_ref().is_some_and(|phone| phone.picker),
                    Modal::PhonePicker,
                ),
                // The migration's agent pane beside it takes typing.
                (
                    self.onboarding.open && !self.onboarding.migrating(),
                    Modal::Onboarding,
                ),
                (self.connection.find_query.focused, Modal::FindField),
            ]
            .into_iter()
            .find_map(|(open, modal)| open.then_some(modal))
        })
    }

    /// Every key nothing else took. True consumes it.
    pub fn key(&mut self, event: &rune::KeyEvent, tx: &mut Tx) -> bool {
        // Opening takes the release, so the window builds the composer now.
        if self.chief_tap(event, tx) {
            return true;
        }
        if let rune::Key::Modifier { .. } = event.key {
            self.modifiers_changed(event.mods.cmd);
            return false;
        }
        phux_perf::echo_hop::record("stdin");
        if self.pane_number_key(event) {
            return true;
        }
        let action = keys::shortcut(&event.key, event.mods, tx.physical_key());
        if let Some(modal) = self.modal() {
            self.modal_key(modal, event, action, tx);
            return true;
        }
        // A focused control owns its keys, including ones it did not handle.
        // Explicit app chords still work, but typing must not leak to the pane.
        if tx.focused().is_some() && action.is_none() {
            if event.key == rune::Key::Escape {
                tx.blur();
                return true;
            }
            return false;
        }
        if action == Some(Shortcut::ClearSelection) && self.clear_copy_selection() {
            return true;
        }
        if self.copy_mode_key(event, tx) {
            return true;
        }
        if self.native_text_shortcut(event) {
            return false;
        }
        if let Some(action) = action {
            self.shortcut(action, tx);
            return true;
        }
        if self.document_key(event) {
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
        if event.key == rune::Key::Escape && self.dismiss_surface(modal) {
            return;
        }
        if self.modal_content_key(modal, event, action, tx)
            || self.navigation_sheet_key(modal, event)
            || self.modal_field_key(modal, event, action, tx)
        {
            return;
        }
        if let Some(action) = action {
            self.modal_command(modal, action, tx);
        }
    }

    fn dismiss_surface(&mut self, modal: Modal) -> bool {
        match modal {
            Modal::ContextMenu => self.close_menu(),
            Modal::Rename => self.cancel_rename(),
            Modal::News => self.close_news(),
            Modal::Chief => self.close_chief(),
            _ => return false,
        }
        true
    }

    fn modal_content_key(
        &mut self,
        modal: Modal,
        event: &rune::KeyEvent,
        action: Option<Shortcut>,
        tx: &mut Tx,
    ) -> bool {
        match modal {
            Modal::PhoneNavigation => {
                self.phone_navigation_key(event);
            }
            Modal::Changes => self.changes_key(event, tx),
            Modal::Popup if action.is_none() => {
                self.type_key(event, tx);
            }
            Modal::Inbox => return self.inbox_key(event, tx),
            Modal::Onboarding => return self.onboarding_key(event),
            _ => return false,
        }
        true
    }

    fn modal_field_key(
        &mut self,
        modal: Modal,
        event: &rune::KeyEvent,
        action: Option<Shortcut>,
        tx: &mut Tx,
    ) -> bool {
        match modal {
            Modal::Machines => {
                self.machines_key(event, tx);
            }
            Modal::Settings if action.is_none() => self.settings_key(event, tx),
            Modal::Confirm => self.confirm_bar_key(event, tx),
            Modal::PhonePicker => {
                self.phone_picker_key(event);
            }
            _ => return false,
        }
        true
    }

    /// A command while `modal` has the keyboard, from a chord or a menu pick.
    /// Whatever the field or sheet doesn't take is a command or nothing, never
    /// terminal input (Ctrl-C must not reach the job from the find field).
    pub(super) fn modal_command(&mut self, modal: Modal, action: Shortcut, tx: &mut Tx) {
        if !modal.allows(action) {
            return;
        }
        if modal == Modal::Settings && action == Shortcut::Find {
            self.focus_settings_search();
            return;
        }
        if modal == Modal::Palette {
            tx.blur();
        }
        self.shortcut(action, tx);
    }

    /// A key while the confirm bar holds the keyboard: an answer or nothing,
    /// never an edit to the field underneath or terminal input.
    pub(super) fn confirm_bar_key(&mut self, event: &rune::KeyEvent, tx: &mut Tx) {
        if self.clipboard_read_key(event, tx).is_none()
            && self.clipboard_write_key(event, tx).is_none()
            && self.open_url_key(event, tx).is_none()
            && self.agent_sign_key(event, tx).is_none()
        {
            self.confirm_key(event);
        }
    }

    /// The confirm bar's keys. True when `event` answered it.
    pub(super) fn confirm_key(&mut self, event: &rune::KeyEvent) -> bool {
        match event.key {
            rune::Key::Enter => self.confirm_pending(),
            rune::Key::Escape => self.pending = None,
            _ => return false,
        }
        true
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
        self.send_typed(&typed);
        true
    }

    /// Send what a keyboard typed, drawn ahead of a remote server's echo.
    pub(super) fn send_typed(&mut self, typed: &Typed) {
        self.send_input(echo::guess(typed), |control, terminal| match typed {
            Typed::Text(text) => control.send_text(terminal, text),
            Typed::Key(atom) => control.send_key(terminal, atom.clone()),
        });
    }

    /// Send typed input to the focused terminal, whole or not at all: it
    /// returns to the live tail first and clears a stale highlight, as
    /// Ghostty does. Input the connection can't take now is said, not dropped
    /// quietly. With `sync-panes` on, the tab's other terminals get it too.
    ///
    /// `guess` is what the keys should put on screen. It is drawn at once
    /// on a terminal whose echo crosses a network (`[experimental]
    /// predictive-echo`), and the server's echo confirms or replaces it.
    pub(super) fn send_input(
        &mut self,
        guess: Option<&str>,
        send: impl Fn(&mut ControlPlane, &ResourceId) -> bool,
    ) {
        self.stop_coast();
        self.leave_copy_mode(false);
        let (Some(client), Some((terminal, Some(view)))) =
            (self.connection.client.clone(), self.keyboard())
        else {
            return;
        };
        let terminal = terminal.clone();
        // Only this computer's server echoes inside a frame; anything else
        // is a round trip away.
        let local =
            self.machines.current.is_none() && self.native.is_none() && terminal.host().is_none();
        let guess = guess.filter(|_| self.predictive_echo.unwrap_or(!local));
        let others = self.sync_others(&terminal);
        let frame = client.acquire_view(view);
        let behind = frame.as_ref().is_some_and(|f| !f.scrollbar.at_tail());
        let sent = client.with_control(|control| {
            if !control.input_ready(&terminal) {
                control.trace_input(false);
                return false;
            }
            if behind {
                let _ = control.scroll_view(view, Scroll::Bottom);
            }
            let sent = send(control, &terminal);
            control.trace_input(sent);
            if let Some(text) = guess.filter(|_| sent) {
                let _ = control.predict_view_text(view, text);
            }
            for other in &others {
                if control.input_ready(other) {
                    send(control, other);
                }
            }
            sent
        });
        if !sent {
            self.input_refusals += 1;
            return self.say(NoticeKind::Error, INPUT_PAUSED);
        }
        phux_perf::echo_hop::record("send");
        // A remote server would stall every key for nothing.
        if let Some(before) = frame.as_ref().filter(|_| local) {
            echo::await_echo(&client, view, before);
        }
        self.nudge_typing(&client);
        self.reclaim_fits();
        self.dismiss_notices(|n| n.text == INPUT_PAUSED);
        if frame.is_some_and(|f| has_selection(&f))
            && let Some(engine) = client.engine()
        {
            let _ = engine.clear_view_selection(view);
        }
    }

    /// Rune falls back to the app for IME when the focused control is not a
    /// text field. That doesn't make the terminal the keyboard owner.
    pub(crate) fn runtime_ime(&mut self, event: &rune::Ime, tx: &Tx) -> bool {
        if tx.focused().is_some() {
            self.connection.preedit = None;
            return false;
        }
        self.ime(event)
    }

    /// What the input method composed: dead keys, CJK, the emoji picker.
    /// Plain characters arrive as keys instead. While something else has the
    /// keyboard none of it is the terminal's.
    pub fn ime(&mut self, event: &rune::Ime) -> bool {
        if self.modal().is_some()
            || self
                .connection
                .shown
                .as_ref()
                .is_none_or(|s| s.view.is_none())
        {
            self.connection.preedit = None;
            return false;
        }
        match event {
            rune::Ime::Preedit(text, ..) => {
                self.connection.preedit = Some(text.clone()).filter(|t| !t.is_empty());
            }
            rune::Ime::Commit(text) => {
                self.connection.preedit = None;
                self.restart_blink();
                self.send_input(Some(text), |control, terminal| {
                    control.send_text(terminal, text)
                });
            }
            // Select reconverts editable text, not server-owned terminal
            // output. Enabled/Disabled aren't terminal input either.
            _ => return false,
        }
        true
    }

    /// The user's paste into `terminal`. One that would type line breaks into
    /// a program without bracketed paste waits for confirmation.
    pub(super) fn paste(&mut self, terminal: &ResourceId, text: &str) {
        if self.is_document(terminal) {
            return;
        }
        if text.is_empty() {
            return;
        }
        let Some(client) = &self.connection.client else {
            return;
        };
        let bracketed = client
            .engine()
            .is_some_and(|engine| engine.bracketed_paste(terminal).unwrap_or(false));
        if paste_needs_confirmation(bracketed, text) {
            self.pending = Some(Pending::Paste(terminal.clone(), text.to_owned()));
            return;
        }
        self.send_paste(terminal, text);
    }

    pub fn confirm_pending(&mut self) {
        match self.pending.take() {
            Some(Pending::Mirror(source, target)) => self.start_mirror(source, &target),
            Some(Pending::Paste(terminal, text)) => self.send_paste(&terminal, &text),
            // Answered through `confirm_pending_tx`, which can reach the
            // clipboard and the browser.
            Some(
                Pending::ClipboardRead(..)
                | Pending::ClipboardWrite(..)
                | Pending::OpenUrl(_)
                | Pending::SshSign(_),
            ) => {}
            Some(Pending::Close(terminal)) => self.close_now(&terminal),
            Some(Pending::CloseTab(terminals)) => {
                let terminals = terminals
                    .into_iter()
                    .filter(|id| !self.is_document(id))
                    .collect();
                self.request_terminal_close(terminals);
            }
            Some(Pending::CloseSession(key, _)) => {
                if let Some(client) = &self.connection.client
                    && let Err(error) = client.kill_session(key)
                {
                    self.say(NoticeKind::Error, format!("Session not closed: {error}"));
                }
            }
            None => {}
        }
    }

    pub fn cancel_pending(&mut self) {
        self.decline_open_url();
        self.pending = None;
    }

    /// To the terminal it was meant for, wherever the keyboard went since.
    fn send_paste(&mut self, terminal: &ResourceId, text: &str) {
        if self.connection.topology.is_some() && !self.lists(terminal) {
            self.say(
                NoticeKind::Error,
                "That terminal closed; nothing was pasted.",
            );
            return;
        }
        if let Err(why) = self.deliver_paste(terminal, text) {
            self.say(NoticeKind::Error, why);
        }
    }

    /// Paste as the user, remembering the receipt so a refused or unknown
    /// delivery reaches them. `Err` says why nothing was written: the
    /// terminal can't take input now, or the paste was refused outright
    /// (behind an unresolved one, say), which is never reported as sent.
    pub(super) fn deliver_paste(
        &mut self,
        terminal: &ResourceId,
        text: &str,
    ) -> Result<(), &'static str> {
        const OFFLINE: &str = "Not connected; nothing was pasted.";
        if self.is_document(terminal) {
            return Err(OFFLINE);
        }
        let Some(client) = self.connection.client.clone() else {
            return Err(OFFLINE);
        };
        if !client.input_ready(terminal) {
            return Err(OFFLINE);
        }
        // The server brackets it per DEC 2004 and classifies it. A paste too
        // large for acknowledged input goes as one atomic event, unreceipted.
        // A refusal's receipt is left untracked: the caller says it instead.
        let receipt = client.with_control(|control| control.apply_user_paste(terminal, text));
        client.wake();
        let receipt = receipt.map_err(|_| "Paste refused; nothing was written.")?;
        self.connection.pastes.extend(receipt);
        self.reclaim_fits();
        Ok(())
    }

    /// One acknowledged write resolved. Unknown is never retried: only the
    /// person can see whether it landed.
    pub(super) fn paste_delivered(&mut self, delivery: u64, outcome: DeliveryOutcome) -> bool {
        if !self.connection.pastes.remove(&delivery) {
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
        let (Some(client), Some(shown)) = (&self.connection.client, &self.connection.shown) else {
            return;
        };
        let Some(view) = shown.view else { return };
        let _ = client.with_control(|control| control.scroll_view(view, scroll));
        self.generation += 1;
    }

    /// Scroll the focused pane by `halves` half pages; a page is its height
    /// less a row, as copy mode pages.
    pub(super) fn scroll_page(&mut self, halves: i64) {
        let view = self.connection.shown.as_ref().and_then(|s| s.view);
        let rows = view
            .and_then(|view| self.connection.client.as_ref()?.acquire_view(view))
            .map_or(1, |frame| frame.rows);
        let page = i64::from(rows.saturating_sub(1).max(1));
        let rows = if halves.abs() == 2 {
            page
        } else {
            (page / 2).max(1)
        };
        self.scroll(Scroll::Delta(rows * halves.signum()));
    }

    /// Back to the live tail of a pane scrolled into history.
    pub fn follow_live(&mut self, terminal: &ResourceId) {
        let (Some(client), Some(view)) = (self.connection.client.clone(), self.view_of(terminal))
        else {
            return;
        };
        let _ = client.with_control(|control| control.scroll_view(view, Scroll::Bottom));
        self.generation += 1;
    }

    /// The view showing `terminal`, focused or not.
    pub fn view_of(&self, terminal: &ResourceId) -> Option<ViewId> {
        match &self.connection.shown {
            Some(shown) if &shown.resource == terminal => shown.view,
            _ => (self.connection.others.get(terminal).copied().flatten())
                .or_else(|| self.connection.popup.view_of(terminal)),
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod runtime;
