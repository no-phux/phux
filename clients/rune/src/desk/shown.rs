//! Which terminal is on screen: asking for one, opening its view, and
//! carrying that across a reconnect.

use std::time::Instant;

use phux_client_runtime::control::Status;
use phux_client_runtime::{Client, ViewId};
use phux_protocol::ids::ResourceId;

use super::{Desk, NoticeKind, Shown, WANTED_GRACE, input, sessions};

// View leases end with their window, including an embedded native host.
impl Drop for Desk {
    fn drop(&mut self) {
        self.release_connection_views();
    }
}

impl Desk {
    /// Shared by window destruction and explicit runtime replacement. Keeping
    /// external client clones alive must not keep this window's leases alive.
    pub(super) fn release_connection_views(&mut self) {
        let Some(client) = self.connection.client.clone() else {
            return;
        };
        // Window teardown retires presentation, never durable execution.
        self.close_popup(&client, false);
        if let Some(view) = self.connection.shown.take().and_then(|s| s.view) {
            let _ = client.destroy_view(view);
        }
        for view in self.connection.others.drain().filter_map(|(_, v)| v) {
            let _ = client.destroy_view(view);
        }
    }
    pub(super) fn want(&mut self, terminal: ResourceId) {
        let Some(client) = self.connection.client.as_ref() else {
            return;
        };
        let intent = super::view_intent::ViewIntent::new(
            client,
            &terminal,
            self.connection.view_server.clone(),
            self.connection.view_epoch,
        );
        self.connection
            .view_intents
            .insert(terminal.clone(), intent);
        self.connection.view_choice += 1;
        self.request_terminal(terminal);
    }

    fn request_terminal(&mut self, terminal: ResourceId) {
        self.retry_document(&terminal);
        self.connection.resources.returning = None;
        if let Some(document) = self.connection.resources.entries.get(&terminal) {
            self.connection
                .resources
                .dismissed
                .remove(&document.record.key);
        }
        self.connection.wanted = Some((terminal, Instant::now()));
    }

    /// Recovery can show a fallback without replacing the person's last choice.
    pub(super) fn want_automatic(&mut self, terminal: ResourceId) {
        let returning = self.connection.resources.returning;
        if !self.connection.view_intents.contains_key(&terminal) {
            let Some(client) = self.connection.client.as_ref() else {
                return;
            };
            let intent = super::view_intent::ViewIntent::new(
                client,
                &terminal,
                self.connection.view_server.clone(),
                self.connection.view_epoch,
            );
            self.connection
                .view_intents
                .insert(terminal.clone(), intent);
        }
        self.request_terminal(terminal);
        self.connection.resources.returning = returning;
    }

    pub(super) fn drop_shown(&mut self, client: &Client) -> Option<ResourceId> {
        if let Some(phone) = self.phone.as_mut() {
            phone.reset_navigation();
        }
        let shown = self.connection.shown.take()?;
        if let Some(view) = shown.view {
            let _ = client.destroy_view(view);
        }
        self.connection.fitted.remove(&shown.resource);
        self.connection.painted.replace(None);
        Some(shown.resource)
    }

    pub(super) fn close_shown(&mut self, client: &Client, excluded: &[ResourceId]) {
        let next = self.phone_close_target(client, excluded);
        self.drop_shown(client);
        // Fall back to whatever the server now focuses.
        self.connection.wanted = None;
        if let Some(next) = next {
            self.want_automatic(next);
        }
    }

    /// Mirrors and topology may still describe the beginning of a coalesced
    /// batch. Choose from the retained phone group, but validate against the
    /// latest organization and terminal roster, excluding the whole hide batch.
    fn phone_close_target(&self, client: &Client, excluded: &[ResourceId]) -> Option<ResourceId> {
        let tree = self.phone.as_ref()?.tree.as_ref()?;
        let shown = self
            .connection
            .shown
            .as_ref()
            .map(|shown| &shown.resource)
            .or_else(|| self.connection.wanted.as_ref().map(|(id, _)| id))?;
        let all = phux_client_core::layout::leaves(tree);
        let at = all.iter().position(|id| id == shown)?;
        let org = client.organization();
        if !org.is_live() {
            return None;
        }
        let topology = client.topology();
        all[..at]
            .iter()
            .rev()
            .chain(all[at + 1..].iter())
            .filter(|id| !excluded.contains(id))
            .find(|id| {
                let Some(record) = org.resource_by_live(id) else {
                    return false;
                };
                if self.connection.resources.dismissed.contains(&record.key) {
                    return false;
                }
                if record.kind.is_terminal() {
                    return topology.as_ref().is_some_and(|t| t.pane(id).is_some());
                }
                record.kind.is_document() && !self.connection.resources.execution_closed(record)
            })
            .cloned()
    }

    /// A transport epoch is not a publication replacement. Retained offline
    /// views stay valid; dead views retry only their original source below.
    pub(super) fn reopen_after_reconnect(&mut self, _client: &Client) -> bool {
        self.connection.fetched.clear();
        self.connection.fitted.clear();
        false
    }

    /// Let go of views the engine forgot. A terminal whose replica leaves
    /// and comes back (a resync, a detach) loses every view on it, and a
    /// dead view paints "Opening…" forever. The shown one is asked for again
    /// and the layout reopens the rest, as soon as the replica is back.
    pub(super) fn drop_dead_views(&mut self, client: &Client) -> bool {
        let dead = |view| client.view_generation(view).is_none();
        let before = self.connection.others.len();
        self.connection
            .others
            .retain(|_, view| view.is_none_or(|view| !dead(view)));
        let mut changed = self.connection.others.len() != before;
        if self
            .connection
            .shown
            .as_ref()
            .and_then(|s| s.view)
            .is_some_and(dead)
            && let Some(terminal) = self.drop_shown(client)
        {
            if self.connection.wanted.is_none() {
                self.want_automatic(terminal);
            }
            changed = true;
        }
        changed
    }

    /// Which terminal should be on screen, if it is not yet: the one the user
    /// asked for, a new session's once it is selected, else the server's
    /// focus. `None` also while waiting on one of those.
    pub(super) fn target(&mut self, client: &Client) -> Option<ResourceId> {
        self.connection.topology.as_ref()?;
        if let Some(target) = self.wanted_target() {
            return Some(target);
        }
        if self.connection.pending_session.is_some() {
            return self.new_session_target(client);
        }
        if self.connection.shown.is_some() {
            return None;
        }
        sessions::default_pane(
            self.connection.topology.as_ref()?,
            client.selected_session()?,
        )
        .or_else(|| self.default_document(client.selected_session()?))
    }

    fn wanted_target(&mut self) -> Option<ResourceId> {
        if let Some((terminal, since)) = &self.connection.wanted {
            let listed = self.lists_resource(terminal);
            if listed || since.elapsed() < WANTED_GRACE {
                return Some(terminal.clone());
            }
            // Gone (a restarted server, a closed pane): fall back below.
            self.connection.wanted = None;
        }
        None
    }

    fn new_session_target(&mut self, client: &Client) -> Option<ResourceId> {
        if !self
            .connection
            .pending_session_origin
            .as_ref()
            .is_some_and(|origin| origin.is_current(&self.connection))
        {
            self.connection.pending_session = None;
            self.connection.pending_session_origin = None;
            self.link.created = None;
            return None;
        }
        if self.link.creating.is_some() {
            return None;
        }
        let topology = self.connection.topology.as_ref()?;
        let name = self.connection.pending_session.as_ref()?;
        let session = match self.link.created {
            Some(key) => topology
                .sessions
                .iter()
                .find(|session| session.key == Some(key)),
            None => topology
                .sessions
                .iter()
                .find(|session| &session.name == name),
        }?;
        if client.selected_session() != Some(session.id) {
            return None;
        }
        let target = sessions::default_pane(topology, session.id)
            .or_else(|| self.default_document(session.id));
        self.connection.pending_session = None;
        self.connection.pending_session_origin = None;
        self.link.created = None;
        if target.is_none() {
            // An existing persistent session can be empty. Completing its
            // attach must stop showing the previous session's terminal.
            self.drop_shown(client);
        }
        target
    }

    /// Retry the original presentation intent, never a newer incarnation of
    /// the same numeric handle. Only a missing first publication may attach.
    pub(super) fn create_view(&mut self, client: &Client, terminal: &ResourceId) -> Option<ViewId> {
        if !self.connection.owns_client(client) {
            return None;
        }
        if !self.connection.view_intents.contains_key(terminal) {
            let intent = super::view_intent::ViewIntent::new(
                client,
                terminal,
                self.connection.view_server.clone(),
                self.connection.view_epoch,
            );
            self.connection
                .view_intents
                .insert(terminal.clone(), intent);
        }
        let intent = self
            .connection
            .view_intents
            .get_mut(terminal)
            .expect("captured above");
        if let Some(view) = intent.open(client, terminal) {
            return Some(view);
        }
        if intent.refused() {
            return None;
        }
        let here = (self.connection.topology.as_ref())
            .and_then(|topology| topology.pane(terminal))
            .is_some_and(|pane| client.selected_session() == Some(pane.session_id));
        if here && client.status() == Status::Attached {
            intent.subscribe(client, terminal);
        }
        None
    }

    /// What a terminal opening makes old news. A failure stays its full time:
    /// a warning raised by the reconnect that reopened this view is exactly
    /// what the person is about to need.
    fn clear_passing_notice(&mut self) {
        self.dismiss_notices(|n| n.kind == NoticeKind::Info || n.text == input::INPUT_PAUSED);
    }

    /// Open the target terminal once its replica is ready. Retried on every
    /// drain until it succeeds; the runtime streams attached terminals on
    /// its own, so nothing here forces a reconnect.
    pub(super) fn open_wanted(&mut self, client: &Client) -> bool {
        let Some(target) = self.target(client) else {
            return false;
        };
        self.subscribe_document(client, &target);
        if self
            .connection
            .shown
            .as_ref()
            .is_some_and(|s| s.resource == target)
        {
            self.connection.wanted = None;
            return false;
        }
        if self.connection.others.contains_key(&target) {
            let returning = self.connection.resources.returning;
            self.focus_pane(&target);
            self.connection.resources.returning = returning;
            self.connection.wanted = None;
            return true;
        }
        let view = if self.is_document(&target) {
            None
        } else {
            let Some(view) = self.create_view(client, &target) else {
                if self
                    .connection
                    .view_intents
                    .get(&target)
                    .is_some_and(|intent| intent.refused())
                {
                    self.connection.wanted = None;
                    return false;
                }
                // Keep the first asking's clock, or the grace never runs out.
                if self.connection.wanted.is_none() {
                    self.want_automatic(target.clone());
                }
                if let Some(intent) = self.connection.view_intents.get(&target) {
                    intent.select_terminal_session(client, &target);
                }
                return false;
            };
            Some(view)
        };
        // The old focus stays on screen if the layout still shows it.
        if let Some(old) = self.connection.shown.take() {
            self.leaving(&old.resource, &target);
            self.connection.others.insert(old.resource, old.view);
            self.connection.painted.replace(None);
        }
        self.activities.seen(&target);
        self.connection.shown = Some(Shown {
            resource: target,
            view,
        });
        if let Some(phone) = self.phone.as_mut() {
            phone.reset_navigation();
            self.connection.layout = Some(phux_client_core::layout::LayoutNode::Leaf(
                self.connection
                    .shown
                    .as_ref()
                    .expect("just opened")
                    .resource
                    .clone(),
            ));
        }
        self.connection.wanted = None;
        self.clear_passing_notice();
        self.say_link_moved();
        self.generation += 1;
        true
    }
}

#[cfg(test)]
mod reconnect;

#[cfg(test)]
mod unstreamed;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::Resolved;

    fn desk() -> Desk {
        Desk::with_preferences(
            &Resolved::offline("not launched"),
            None,
            rune::prefs::Prefs::memory(),
        )
    }

    #[test]
    fn a_view_the_engine_forgot_is_asked_for_again() {
        let mut desk = desk();
        let client = std::sync::Arc::new(phux_client_runtime::Runtime::embedded(
            phux_client_runtime::control::ControlOptions::default(),
        ));
        desk.connection.client = Some(client.clone());
        let view = |raw| phux_client_runtime::ViewId::from_raw(raw).expect("nonzero");
        // Nothing is published for either view: both died with the replica.
        desk.connection.shown = Some(Shown {
            resource: ResourceId::local(1),
            view: Some(view(1)),
        });
        desk.connection
            .others
            .insert(ResourceId::local(2), Some(view(2)));
        assert!(desk.drop_dead_views(&client));
        assert!(desk.connection.shown.is_none());
        assert!(desk.connection.others.is_empty());
        assert_eq!(
            desk.connection.wanted.as_ref().map(|(t, _)| t),
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

#[cfg(test)]
mod source_admission;
