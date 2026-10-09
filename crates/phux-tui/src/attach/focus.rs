//! Client-local focus transition and one-entry MRU bookkeeping. Focus is
//! consumer-local (ADR-0019): never serialized or sent over the wire.

use phux_protocol::ResourceId;

use crate::layout::{self, Workspace};

/// Client-local pane focus and the session names this client has attached.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct FocusHistory {
    previous: Option<ResourceId>,
    /// Attached session names, oldest first. Not persisted.
    sessions: Vec<String>,
}

impl FocusHistory {
    /// Seed history in focused dispatcher tests.
    #[cfg(test)]
    pub(super) const fn with_previous(previous: ResourceId) -> Self {
        Self {
            previous: Some(previous),
            sessions: Vec::new(),
        }
    }

    /// Remember `name` unless it is empty or already the newest entry.
    pub(super) fn remember_session(&mut self, name: &str) {
        if name.is_empty() {
            return;
        }
        if self.sessions.last().is_none_or(|last| last != name) {
            self.sessions.push(name.to_owned());
        }
    }

    /// Attached session names, oldest first.
    pub(super) fn session_names(&self) -> &[String] {
        &self.sessions
    }

    /// Replace the session history. Dispatcher tests seed a known MRU.
    #[cfg(test)]
    pub(super) fn set_sessions(&mut self, names: Vec<String>) {
        self.sessions = names;
    }

    /// Apply one focus transition and remember the pane being left.
    pub(super) fn transition(
        &mut self,
        current: &mut Option<ResourceId>,
        next: Option<ResourceId>,
    ) {
        if *current != next {
            self.previous.clone_from(current);
            *current = next;
        }
    }

    /// Record a transition performed by an async/reconcile helper that owns
    /// the focused pointer while it runs.
    pub(super) fn observe(&mut self, before: Option<ResourceId>, after: Option<&ResourceId>) {
        if before.as_ref() != after {
            self.previous = before;
        }
    }

    /// Return the live jump-back target, clearing stale/self references.
    pub(super) fn target(
        &mut self,
        current: Option<&ResourceId>,
        workspace: &Workspace,
    ) -> Option<ResourceId> {
        self.repair(current, workspace);
        self.previous.clone()
    }

    /// Drop history when its pane closed/disappeared or equals current focus.
    pub(super) fn repair(&mut self, current: Option<&ResourceId>, workspace: &Workspace) {
        let valid = self.previous.as_ref().is_some_and(|previous| {
            Some(previous) != current
                && workspace.windows.iter().any(|window| {
                    window
                        .state
                        .tree
                        .as_ref()
                        .is_some_and(|tree| layout::leaves(tree).contains(previous))
                })
        });
        if !valid {
            self.previous = None;
        }
    }

    #[cfg(test)]
    pub(super) const fn previous(&self) -> Option<&ResourceId> {
        self.previous.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tid(id: u32) -> ResourceId {
        ResourceId::local(id)
    }

    #[test]
    fn repeated_transitions_toggle_and_stale_history_is_cleared() {
        let mut workspace = Workspace::single(tid(1));
        workspace.add_window("2".to_owned(), tid(2));
        workspace.select(0);
        let mut current = Some(tid(1));
        let mut history = FocusHistory::default();

        history.transition(&mut current, Some(tid(2)));
        assert_eq!(history.target(current.as_ref(), &workspace), Some(tid(1)));
        history.transition(&mut current, Some(tid(1)));
        assert_eq!(history.target(current.as_ref(), &workspace), Some(tid(2)));

        workspace.windows.pop();
        history.repair(current.as_ref(), &workspace);
        assert_eq!(history.previous(), None);
    }
}
