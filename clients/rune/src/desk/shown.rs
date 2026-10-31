//! Which terminal is on screen: asking for one, opening its view, and
//! carrying that across a reconnect.

use std::time::Instant;

use phux_client_runtime::Client;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::AttachTarget;

use super::{Desk, Shown, WANTED_GRACE, sessions};

impl Desk {
    pub(super) fn want(&mut self, terminal: ResourceId) {
        self.wanted = Some((terminal, Instant::now()));
    }

    pub(super) fn drop_shown(&mut self, client: &Client) -> Option<ResourceId> {
        let shown = self.shown.take()?;
        let _ = client.destroy_view(shown.view);
        self.fitted.remove(&shown.terminal);
        self.painted.replace(None);
        Some(shown.terminal)
    }

    pub(super) fn close_shown(&mut self, client: &Client) {
        self.drop_shown(client);
        // Fall back to whatever the server now focuses.
        self.wanted = None;
    }

    /// A new connection may be a new server incarnation, whose engine forgot
    /// every view: open the shown terminal again on this connection.
    pub(super) fn reopen_after_reconnect(&mut self, client: &Client) -> bool {
        for (_, view) in self.others.drain() {
            let _ = client.destroy_view(view);
        }
        self.fetched.clear();
        self.fitted.clear();
        let Some(terminal) = self.drop_shown(client) else {
            return false;
        };
        if self.wanted.is_none() {
            self.want(terminal);
        }
        true
    }

    /// Which terminal should be on screen, if it is not yet: the one the user
    /// asked for, a new session's once it is selected, else the server's
    /// focus. `None` also while waiting on one of those.
    pub(super) fn target(&mut self, client: &Client) -> Option<ResourceId> {
        let topology = self.topology.as_ref()?;
        if let Some((terminal, since)) = &self.wanted {
            let listed = topology.panes.iter().any(|p| &p.terminal_id == terminal);
            if listed || since.elapsed() < WANTED_GRACE {
                return Some(terminal.clone());
            }
            // Gone (a restarted server, a closed pane): fall back below.
            self.wanted = None;
        }
        if let Some(name) = &self.pending_session {
            let session = topology.sessions.iter().find(|s| &s.name == name)?;
            if client.selected_session() != Some(session.id) {
                return None;
            }
            let panes = || topology.panes.iter().filter(|p| p.session_id == session.id);
            let pane = panes()
                .find(|p| p.terminal_id == topology.focused_pane)
                .or_else(|| panes().next())?;
            let terminal = pane.terminal_id.clone();
            self.pending_session = None;
            return Some(terminal);
        }
        if self.shown.is_some() {
            return None;
        }
        sessions::default_pane(topology, client.selected_session()?)
    }

    /// Whether `terminal` is in a session this client doesn't stream yet.
    fn foreign(&self, client: &Client, terminal: &ResourceId) -> Option<String> {
        let pane = self.topology.as_ref()?.pane(terminal)?;
        (client.selected_session() != Some(pane.session_id)).then(|| pane.session_name.clone())
    }

    /// Open the target terminal once its replica is ready. Retried on every
    /// drain until it succeeds; the runtime streams attached terminals on
    /// its own, so nothing here forces a reconnect.
    pub(super) fn open_wanted(&mut self, client: &Client) -> bool {
        let Some(target) = self.target(client) else {
            return false;
        };
        if self.shown.as_ref().is_some_and(|s| s.terminal == target) {
            self.wanted = None;
            return false;
        }
        if self.others.contains_key(&target) {
            self.focus_pane(&target);
            self.wanted = None;
            return true;
        }
        let Ok(view) = client.create_view(&target) else {
            // Keep the first asking's clock, or the grace never runs out.
            if self.wanted.is_none() {
                self.want(target.clone());
            }
            if let Some(session) = self.foreign(client, &target) {
                client.attach_session(AttachTarget::ByName(session));
            }
            return false;
        };
        // The old focus stays on screen if the layout still shows it.
        if let Some(old) = self.shown.take() {
            self.others.insert(old.terminal, old.view);
            self.painted.replace(None);
        }
        self.activities.seen(&target);
        self.shown = Some(Shown {
            terminal: target,
            view,
        });
        self.wanted = None;
        self.notice = None;
        self.say_link_moved();
        self.generation += 1;
        true
    }
}
