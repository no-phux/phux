//! Which terminal is on screen: asking for one, opening its view, and
//! carrying that across a reconnect.

use std::time::Instant;

use phux_client_runtime::Client;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::AttachTarget;

use super::{Desk, NoticeKind, Shown, WANTED_GRACE, input, sessions};

impl Desk {
    pub(super) fn want(&mut self, terminal: ResourceId) {
        self.resources.returning = None;
        if let Some(document) = self.resources.entries.get(&terminal) {
            self.resources.dismissed.remove(&document.record.key);
        }
        self.wanted = Some((terminal, Instant::now()));
    }

    pub(super) fn drop_shown(&mut self, client: &Client) -> Option<ResourceId> {
        let shown = self.shown.take()?;
        if let Some(view) = shown.view {
            let _ = client.destroy_view(view);
        }
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
        for view in self.others.drain().filter_map(|(_, v)| v) {
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

    /// Let go of views the engine forgot. A terminal whose replica leaves
    /// and comes back (a resync, a detach) loses every view on it, and a
    /// dead view paints "Opening…" forever. The shown one is asked for again
    /// and the layout reopens the rest, as soon as the replica is back.
    pub(super) fn drop_dead_views(&mut self, client: &Client) -> bool {
        let dead = |view| client.view_generation(view).is_none();
        let before = self.others.len();
        self.others
            .retain(|_, view| view.is_none_or(|view| !dead(view)));
        let mut changed = self.others.len() != before;
        if self.shown.as_ref().and_then(|s| s.view).is_some_and(dead)
            && let Some(terminal) = self.drop_shown(client)
        {
            if self.wanted.is_none() {
                self.want(terminal);
            }
            changed = true;
        }
        changed
    }

    /// Which terminal should be on screen, if it is not yet: the one the user
    /// asked for, a new session's once it is selected, else the server's
    /// focus. `None` also while waiting on one of those.
    pub(super) fn target(&mut self, client: &Client) -> Option<ResourceId> {
        self.topology.as_ref()?;
        if let Some(target) = self.wanted_target() {
            return Some(target);
        }
        if self.pending_session.is_some() {
            return self.new_session_target(client);
        }
        if self.shown.is_some() {
            return None;
        }
        sessions::default_pane(self.topology.as_ref()?, client.selected_session()?)
            .or_else(|| self.default_document(client.selected_session()?))
    }

    fn wanted_target(&mut self) -> Option<ResourceId> {
        if let Some((terminal, since)) = &self.wanted {
            let listed = self.lists_resource(terminal);
            if listed || since.elapsed() < WANTED_GRACE {
                return Some(terminal.clone());
            }
            // Gone (a restarted server, a closed pane): fall back below.
            self.wanted = None;
        }
        None
    }

    fn new_session_target(&mut self, client: &Client) -> Option<ResourceId> {
        let topology = self.topology.as_ref()?;
        let name = self.pending_session.as_ref()?;
        let session = topology.sessions.iter().find(|s| &s.name == name)?;
        if client.selected_session() != Some(session.id) {
            return None;
        }
        let target = sessions::default_pane(topology, session.id)
            .or_else(|| self.default_document(session.id))?;
        self.pending_session = None;
        Some(target)
    }

    /// Whether `terminal` is in a session this client doesn't stream yet.
    fn foreign(&self, client: &Client, terminal: &ResourceId) -> Option<String> {
        let pane = self.topology.as_ref()?.pane(terminal)?;
        (client.selected_session() != Some(pane.session_id)).then(|| pane.session_name.clone())
    }

    /// What a terminal opening makes old news. A failure stays its full time:
    /// a warning raised by the reconnect that reopened this view is exactly
    /// what the person is about to need.
    fn clear_passing_notice(&mut self) {
        self.notice
            .take_if(|n| n.kind == NoticeKind::Info || n.text == input::INPUT_PAUSED);
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
        let view = if self.is_document(&target) {
            None
        } else {
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
            Some(view)
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
        self.clear_passing_notice();
        self.say_link_moved();
        self.generation += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::Resolved;

    fn desk() -> Desk {
        Desk::new(&Resolved::offline("not launched"))
    }

    #[test]
    fn a_view_the_engine_forgot_is_asked_for_again() {
        let mut desk = desk();
        let client = std::sync::Arc::new(phux_client_runtime::Runtime::embedded(
            phux_client_runtime::control::ControlOptions::default(),
        ));
        let view = |raw| phux_client_runtime::ViewId::from_raw(raw).expect("nonzero");
        // Nothing is published for either view: both died with the replica.
        desk.shown = Some(Shown {
            terminal: ResourceId::local(1),
            view: Some(view(1)),
        });
        desk.others.insert(ResourceId::local(2), Some(view(2)));
        assert!(desk.drop_dead_views(&client));
        assert!(desk.shown.is_none());
        assert!(desk.others.is_empty());
        assert_eq!(
            desk.wanted.as_ref().map(|(t, _)| t),
            Some(&ResourceId::local(1))
        );
        assert!(!desk.drop_dead_views(&client), "nothing left to drop");
    }

    #[test]
    fn a_terminal_opening_leaves_a_warning_up() {
        let mut desk = desk();
        let warning = "Paste may not have landed; look before pasting again.";
        desk.say(NoticeKind::Error, warning);
        desk.clear_passing_notice();
        assert_eq!(desk.notice.as_ref().map(|n| n.text.as_str()), Some(warning));
    }

    #[test]
    fn a_terminal_opening_clears_what_was_in_passing() {
        let mut desk = desk();
        desk.say(NoticeKind::Info, "Diagnostics copied.");
        desk.clear_passing_notice();
        assert_eq!(desk.notice, None);
        desk.say(NoticeKind::Error, input::INPUT_PAUSED);
        desk.clear_passing_notice();
        assert_eq!(desk.notice, None);
    }
}
