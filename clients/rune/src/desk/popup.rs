//! A popup: one command in a floating terminal over the window, gone when
//! the command exits.
//!
//! Its terminal is an ordinary one in the focused terminal's session that
//! this window never writes into the arrangement, as the terminal UI's
//! popup does: it is no tab and no split, here or in another client.

use std::time::Duration;

use phux_client_runtime::control::SpawnRequest;
use phux_client_runtime::{Client, ViewId};
use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::{Desk, NoticeKind, Pending};
use crate::palette::{Item, Mode, Target};

#[derive(Debug, Default)]
pub struct Popup {
    /// The command line, while the box is up.
    command: Option<String>,
    /// The focused terminal's folder and session when it was asked for.
    cwd: Option<String>,
    session: Option<u32>,
    /// The spawn awaiting its correlated answer. Dismissing the box does not
    /// compensate by terminating a durable execution that may already exist.
    request: Option<u32>,
    /// Its terminal, and that terminal's view once the replica has one.
    terminal: Option<(ResourceId, Option<ViewId>)>,
    /// Captured when the command was requested, not when a receipt arrives.
    server: Option<Vec<u8>>,
    intent: Option<super::view_intent::ViewIntent>,
    origin_epoch: u64,
    origin: Option<super::connection::CallbackFence>,
}

impl Popup {
    pub(in crate::desk) fn owns_request(&self, request: u32) -> bool {
        self.request == Some(request)
    }
    /// The command line of the box on screen.
    pub fn command(&self) -> Option<&str> {
        self.command.as_deref()
    }

    pub fn terminal(&self) -> Option<&ResourceId> {
        self.terminal.as_ref().map(|(id, _)| id)
    }

    pub(super) fn view_of(&self, terminal: &ResourceId) -> Option<ViewId> {
        let (id, view) = self.terminal.as_ref()?;
        view.filter(|_| id == terminal)
    }

    const fn busy(&self) -> bool {
        self.command.is_some() || self.request.is_some()
    }
}

/// The command a typed line names, or `None` when it names nothing.
pub fn command_line(typed: &str) -> Option<&str> {
    Some(typed.trim()).filter(|line| !line.is_empty())
}

/// The palette's row running the typed line.
pub fn item(query: &str) -> Option<Item> {
    command_line(query).map(|line| Item {
        title: format!("Run \u{201c}{line}\u{201d} in a Popup"),
        detail: "sh -c".to_owned(),
        kind: "action",
        target: Target::Popup(line.to_owned()),
    })
}

/// Why a popup can't start now, in the notice's words. `focused` is the
/// terminal with the keyboard, when it is one.
fn refusal(
    busy: bool,
    phone: bool,
    up: bool,
    focused: Option<&ResourceId>,
) -> Option<&'static str> {
    match focused {
        _ if busy => Some("A popup is already open. Close it first."),
        _ if phone => Some("A popup needs the desktop window."),
        _ if !up => Some("Not connected; a popup can't start."),
        None => Some("A popup needs a focused terminal."),
        // ponytail: this window's spawn can't name a hub's satellite, so it
        // would run here, not there; carry `satellite` in `SpawnRequest`
        // when popups should reach those terminals.
        Some(id) if id.host().is_some() => {
            Some("A popup can't start on that machine's terminal from here.")
        }
        Some(_) => None,
    }
}

impl Desk {
    /// The terminal typing, paste and copy belong to: the popup's while its
    /// box is up, else the focused one.
    pub fn keyboard(&self) -> Option<(&ResourceId, Option<ViewId>)> {
        if self.connection.popup.command.is_some() {
            return self
                .connection
                .popup
                .terminal
                .as_ref()
                .map(|(id, view)| (id, *view));
        }
        self.connection
            .shown
            .as_ref()
            .map(|s| (&s.resource, s.view))
    }

    fn popup_refusal(&self) -> Option<&'static str> {
        let focused = self.connection.shown.as_ref().map(|s| &s.resource);
        refusal(
            self.connection.popup.busy(),
            self.phone.is_some(),
            self.link_is_up(),
            focused.filter(|id| !self.is_document(id)),
        )
    }

    /// Ask for the command line, where a popup could start.
    pub(super) fn ask_popup(&mut self) {
        match self.popup_refusal() {
            Some(why) => self.say(NoticeKind::Info, why),
            None => self.open_palette(Mode::Popup),
        }
    }

    /// Put the box up for `typed`. Its command starts once the box is laid
    /// out, at the size it will have.
    pub(super) fn open_popup(&mut self, typed: &str) {
        if let Some(why) = self.popup_refusal() {
            return self.say(NoticeKind::Info, why);
        }
        let Some(line) = command_line(typed) else {
            return;
        };
        self.connection.popup.command = Some(line.to_owned());
        self.connection.popup.cwd = self.focused_cwd();
        self.connection.popup.session = self
            .connection
            .client
            .as_ref()
            .and_then(|c| c.selected_session());
        self.connection.popup.server = self.connection.view_server.clone();
        self.connection.popup.origin_epoch = self.connection.view_epoch;
        self.connection.popup.origin = self.connection.callback_fence();
    }

    /// The empty box's grid area in points: run the command through `sh`,
    /// in the focused terminal's folder and session, at that many cells.
    pub fn popup_laid_out(&mut self, area: [f32; 2]) {
        let (Some(client), Some(line)) = (
            self.connection.client.clone(),
            self.connection.popup.command.clone(),
        ) else {
            return;
        };
        if self.connection.popup.request.is_some() || self.connection.popup.terminal.is_some() {
            return;
        }
        if !self
            .connection
            .popup
            .origin
            .as_ref()
            .is_some_and(|origin| origin.is_current(&self.connection))
        {
            self.close_popup(&client, false);
            return;
        }
        let request = SpawnRequest {
            command: Some(vec!["/bin/sh".to_owned(), "-c".to_owned(), line]),
            cwd: self.connection.popup.cwd.clone(),
            env: None,
            session_id: self.connection.popup.session,
            initial_size: Some(self.metrics().fit(area)),
        };
        let submission = client.with_control(|control| {
            (control.connection_epoch() == self.connection.popup.origin_epoch
                && control.server().map(|server| server.id.as_slice())
                    == self.connection.popup.server.as_deref())
            .then(|| control.submit_spawn(request))
        });
        let Some(submission) = submission else {
            self.close_popup(&client, false);
            return;
        };
        self.connection.popup.request = self.accept_spawn(submission);
    }

    /// The server's answer to a spawn. True when it was the popup's.
    pub(super) fn popup_spawned(
        &mut self,
        client: &Client,
        request: u32,
        id: Option<&ResourceId>,
        error: Option<&str>,
    ) -> bool {
        if !self.connection.owns_client(client) {
            return false;
        }
        if self.connection.popup.request != Some(request) {
            return false;
        }
        self.connection.popup.request = None;
        self.connection.spawn_servers.remove(&request);
        match (id, self.connection.popup.command.is_some()) {
            (Some(id), true) => {
                let mut intent = super::view_intent::ViewIntent::new(
                    client,
                    id,
                    self.connection.popup.server.clone(),
                    self.connection.popup.origin_epoch,
                );
                let view = intent.open(client, id);
                if intent.refused() {
                    self.close_popup(client, false);
                    return true;
                }
                self.connection.popup.intent = Some(intent);
                self.connection.popup.terminal = Some((id.clone(), view));
            }
            // No view was installed. The connection's pump is not ours to
            // detach: another view may already be using this exact resource.
            (Some(_), false) => {}
            (None, up) => {
                self.connection.popup = Popup::default();
                if up {
                    let why = error.unwrap_or("spawn failed");
                    self.say(NoticeKind::Error, format!("Popup not started: {why}"));
                }
            }
        }
        true
    }

    /// Retry only the popup's original publication, including offline replicas.
    pub(super) fn tend_popup(&mut self, client: &Client, tx: &mut Tx) -> bool {
        let Some((id, view)) = &mut self.connection.popup.terminal else {
            return false;
        };
        if view.is_some_and(|view| client.view_generation(view).is_some()) {
            return false;
        }
        let Some(intent) = self.connection.popup.intent.as_mut() else {
            self.close_popup(client, false);
            return true;
        };
        *view = intent.open(client, id);
        let opened = view.is_some();
        if intent.refused() {
            self.close_popup(client, false);
            return true;
        }
        if !opened {
            tx.wake_after(Duration::from_millis(250));
        }
        opened
    }

    /// An exit/close notification retires presentation, never authorizes a
    /// compensating execution termination.
    pub(super) fn popup_ended(&mut self, client: &Client, id: &ResourceId) -> bool {
        if !self.connection.owns_client(client) {
            return false;
        }
        if self.connection.popup.terminal() != Some(id) {
            return false;
        }
        self.close_popup(client, false);
        true
    }
    pub(in crate::desk) fn retire_stale_popup(&mut self, client: &Client) -> bool {
        if self.connection.popup.command.is_some()
            && (self.connection.status.is_terminal()
                || self.connection.popup.server != self.connection.view_server
                || (self.connection.popup.origin_epoch != self.connection.epoch
                    && self
                        .connection
                        .popup
                        .intent
                        .as_ref()
                        .is_none_or(|intent| !intent.has_source())))
        {
            self.close_popup(client, false);
            return true;
        }
        false
    }

    /// Close the box and end its command, asking first where closing any
    /// terminal would.
    pub fn dismiss_popup(&mut self) {
        let Some(client) = self.connection.client.clone() else {
            return;
        };
        match self.connection.popup.terminal().cloned() {
            Some(id) if self.asks_to_close(&id) => self.pending = Some(Pending::Close(id)),
            _ => self.close_popup(&client, true),
        }
    }

    /// Take the box down; `kill` ends its command too.
    pub(super) fn close_popup(&mut self, client: &Client, kill: bool) {
        let popup = std::mem::take(&mut self.connection.popup);
        if let Some(request) = popup.request {
            self.connection.spawn_servers.remove(&request);
        }
        if let Some((id, view)) = popup.terminal {
            if let Some(view) = view {
                let _ = client.destroy_view(view);
            }
            if kill
                && popup
                    .intent
                    .as_ref()
                    .is_some_and(|intent| intent.owns_client(client))
            {
                let request = client.with_control(|control| {
                    if control.connection_epoch() == popup.origin_epoch
                        && control.server().map(|server| server.id.as_slice())
                            == popup.server.as_deref()
                    {
                        control.close_terminals(vec![id.clone()])
                    } else {
                        None
                    }
                });
                self.report_close_submission(request);
            }
            self.connection.fitted.remove(&id);
            self.connection.areas.remove(&id);
            self.connection.origins.remove(&id);
            // A question about a box that is gone has nothing to answer.
            self.pending
                .take_if(|pending| *pending == Pending::Close(id));
        }
        self.generation += 1;
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod source_admission;
