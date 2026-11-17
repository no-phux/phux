//! Which terminal is on screen: asking for one, opening its view, and
//! carrying that across a reconnect.

use std::time::Instant;

use phux_client_runtime::Client;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::AttachTarget;

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
        // A popup's command ends with the window that showed it.
        self.close_popup(&client, true);
        if let Some(view) = self.connection.shown.take().and_then(|s| s.view) {
            let _ = client.destroy_view(view);
        }
        for view in self.connection.others.drain().filter_map(|(_, v)| v) {
            let _ = client.destroy_view(view);
        }
    }
    pub(super) fn want(&mut self, terminal: ResourceId) {
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
        self.want(terminal);
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

    /// A new connection may be a new server incarnation, whose engine forgot
    /// every view: open the shown terminal again on this connection.
    pub(super) fn reopen_after_reconnect(&mut self, client: &Client) -> bool {
        if let Some(phone) = self.phone.as_mut() {
            phone.reconcile(None, None);
        }
        for view in self.connection.others.drain().filter_map(|(_, v)| v) {
            let _ = client.destroy_view(view);
        }
        self.connection.fetched.clear();
        self.connection.fitted.clear();
        let Some(terminal) = self.drop_shown(client) else {
            return false;
        };
        if self.connection.wanted.is_none() {
            self.want_automatic(terminal);
        }
        true
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
        let topology = self.connection.topology.as_ref()?;
        let name = self.connection.pending_session.as_ref()?;
        let session = topology.sessions.iter().find(|s| &s.name == name)?;
        if client.selected_session() != Some(session.id) {
            return None;
        }
        let target = sessions::default_pane(topology, session.id)
            .or_else(|| self.default_document(session.id));
        self.connection.pending_session = None;
        if target.is_none() {
            // An existing persistent session can be empty. Completing its
            // attach must stop showing the previous session's terminal.
            self.drop_shown(client);
        }
        target
    }

    /// Whether `terminal` is in a session this client doesn't stream yet.
    fn foreign(&self, client: &Client, terminal: &ResourceId) -> Option<String> {
        let pane = self.connection.topology.as_ref()?.pane(terminal)?;
        (client.selected_session() != Some(pane.session_id)).then(|| pane.session_name.clone())
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
            let Ok(view) = client.create_view(&target) else {
                // Keep the first asking's clock, or the grace never runs out.
                if self.connection.wanted.is_none() {
                    self.want_automatic(target.clone());
                }
                if let Some(session) = self.foreign(client, &target) {
                    client.attach_session(AttachTarget::ByName(session));
                }
                return false;
            };
            Some(view)
        };
        // The old focus stays on screen if the layout still shows it.
        if let Some(old) = self.connection.shown.take() {
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
