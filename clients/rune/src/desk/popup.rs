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
    /// The spawn the server hasn't answered. It outlives a dismissed box, so
    /// the terminal that lands is closed rather than left running unseen.
    request: Option<u32>,
    /// Its terminal, and that terminal's view once the replica has one.
    terminal: Option<(ResourceId, Option<ViewId>)>,
}

impl Popup {
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
        if self.popup.command.is_some() {
            return self.popup.terminal.as_ref().map(|(id, view)| (id, *view));
        }
        self.shown.as_ref().map(|s| (&s.resource, s.view))
    }

    fn popup_refusal(&self) -> Option<&'static str> {
        let focused = self.shown.as_ref().map(|s| &s.resource);
        refusal(
            self.popup.busy(),
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
        self.popup.command = Some(line.to_owned());
        self.popup.cwd = self.focused_cwd();
        self.popup.session = self.client.as_ref().and_then(|c| c.selected_session());
    }

    /// The empty box's grid area in points: run the command through `sh`,
    /// in the focused terminal's folder and session, at that many cells.
    pub fn popup_laid_out(&mut self, area: [f32; 2]) {
        let (Some(client), Some(line)) = (&self.client, &self.popup.command) else {
            return;
        };
        if self.popup.request.is_some() || self.popup.terminal.is_some() {
            return;
        }
        // ponytail: an answer that crosses a server restart is taken at its
        // word; fence it with `Receipts`, as splits are, if that bites.
        self.popup.request = Some(client.spawn_terminal(SpawnRequest {
            command: Some(vec!["/bin/sh".to_owned(), "-c".to_owned(), line.clone()]),
            cwd: self.popup.cwd.clone(),
            env: None,
            session_id: self.popup.session,
            initial_size: Some(self.metrics().fit(area)),
        }));
    }

    /// The server's answer to a spawn. True when it was the popup's.
    pub(super) fn popup_spawned(
        &mut self,
        client: &Client,
        request: u32,
        id: Option<&ResourceId>,
        error: Option<&str>,
    ) -> bool {
        if self.popup.request != Some(request) {
            return false;
        }
        self.popup.request = None;
        match (id, self.popup.command.is_some()) {
            (Some(id), true) => {
                self.popup.terminal = Some((id.clone(), client.create_view(id).ok()));
            }
            // Dismissed while it was starting.
            (Some(id), false) => {
                let _ = client.close_terminals(vec![id.clone()]);
            }
            (None, up) => {
                self.popup = Popup::default();
                if up {
                    let why = error.unwrap_or("spawn failed");
                    self.say(NoticeKind::Error, format!("Popup not started: {why}"));
                }
            }
        }
        true
    }

    /// Open the popup's view once its replica is ready, and again when a
    /// reconnect dropped it. True when the box has something new to draw.
    pub(super) fn tend_popup(&mut self, client: &Client, tx: &mut Tx) -> bool {
        let Some((id, view)) = &mut self.popup.terminal else {
            return false;
        };
        if view.is_some_and(|view| client.view_generation(view).is_some()) {
            return false;
        }
        *view = client.create_view(id).ok();
        if view.is_none() {
            tx.wake_after(Duration::from_millis(250));
        }
        view.is_some()
    }

    /// `id`'s command exited (`kill`: the server may keep the terminal for
    /// its exit status, so it is closed) or its terminal closed. True when
    /// it was the popup's, whose box goes with it.
    pub(super) fn popup_ended(&mut self, client: &Client, id: &ResourceId, kill: bool) -> bool {
        if self.popup.terminal() != Some(id) {
            return false;
        }
        self.close_popup(client, kill);
        true
    }

    /// Close the box and end its command, asking first where closing any
    /// terminal would.
    pub fn dismiss_popup(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        match self.popup.terminal().cloned() {
            Some(id) if self.asks_to_close(&id) => self.pending = Some(Pending::Close(id)),
            _ => self.close_popup(&client, true),
        }
    }

    /// Take the box down; `kill` ends its command too.
    pub(super) fn close_popup(&mut self, client: &Client, kill: bool) {
        let popup = std::mem::take(&mut self.popup);
        self.popup.request = popup.request;
        if let Some((id, view)) = popup.terminal {
            if let Some(view) = view {
                let _ = client.destroy_view(view);
            }
            if kill {
                let _ = client.close_terminals(vec![id.clone()]);
            }
            self.fitted.remove(&id);
            self.areas.remove(&id);
            self.origins.remove(&id);
            // A question about a box that is gone has nothing to answer.
            self.pending
                .take_if(|pending| *pending == Pending::Close(id));
        }
        self.generation += 1;
    }
}

#[cfg(test)]
mod tests;
