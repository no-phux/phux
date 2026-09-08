//! Client-local focus transition and one-entry MRU bookkeeping.
//!
//! ADR-0019 makes focus consumer-local. This state therefore lives only for
//! one attached TUI process: it is never serialized into layout metadata or
//! sent over the wire. ADR-0049 (maintained on the sibling focus branch)
//! owns the broader focus model; this module is the implementation dependency
//! for `phux-oih5.4`, not a duplicate decision record.

use phux_protocol::TerminalId;

use crate::layout::{self, Workspace};

/// One-entry, client-local pane focus history.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct FocusHistory {
    previous: Option<TerminalId>,
}

impl FocusHistory {
    /// Seed history in focused dispatcher tests.
    #[cfg(test)]
    pub(super) const fn with_previous(previous: TerminalId) -> Self {
        Self {
            previous: Some(previous),
        }
    }

    /// Apply one focus transition and remember the pane being left.
    pub(super) fn transition(
        &mut self,
        current: &mut Option<TerminalId>,
        next: Option<TerminalId>,
    ) {
        if *current != next {
            self.previous.clone_from(current);
            *current = next;
        }
    }

    /// Record a transition performed by an async/reconcile helper that owns
    /// the focused pointer while it runs.
    pub(super) fn observe(&mut self, before: Option<TerminalId>, after: Option<&TerminalId>) {
        if before.as_ref() != after {
            self.previous = before;
        }
    }

    /// Return the live jump-back target, clearing stale/self references.
    pub(super) fn target(
        &mut self,
        current: Option<&TerminalId>,
        workspace: &Workspace,
    ) -> Option<TerminalId> {
        self.repair(current, workspace);
        self.previous.clone()
    }

    /// Drop history when its pane closed/disappeared or equals current focus.
    pub(super) fn repair(&mut self, current: Option<&TerminalId>, workspace: &Workspace) {
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
    pub(super) const fn previous(&self) -> Option<&TerminalId> {
        self.previous.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tid(id: u32) -> TerminalId {
        TerminalId::local(id)
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
