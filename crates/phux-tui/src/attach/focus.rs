//! Client-local focus transition and one-entry MRU bookkeeping. Focus is
//! consumer-local (ADR-0019): never serialized or sent over the wire. The
//! panes hear about it only as DEC 1004 reports ([`FocusReports`]).

use phux_protocol::ResourceId;
use phux_protocol::input::focus::FocusEvent;

use crate::layout::{self, ArrangementDoc};

/// What the panes have heard about keyboard focus (DEC 1004). The pane that
/// takes keys hears focus-in once; the pane it leaves, by a pane, tab or
/// session switch or the outer window losing focus, hears focus-out. Rune
/// keeps the same rule (`clients/rune/src/desk/sessions.rs`, `sync_focus`).
/// The server drops a report for a pane that hasn't turned 1004 on.
#[derive(Debug)]
pub(super) struct FocusReports {
    /// The pane last told it gained focus.
    told: Option<ResourceId>,
    /// The outer terminal window's focus. A terminal that never reports
    /// counts as focused.
    outer: bool,
}

impl Default for FocusReports {
    fn default() -> Self {
        Self {
            told: None,
            outer: true,
        }
    }
}

impl FocusReports {
    /// Follow the outer terminal window's focus.
    pub(super) const fn set_outer(&mut self, focused: bool) {
        self.outer = focused;
    }

    /// The reports that bring the panes up to date with `keyboard`, the pane
    /// that takes keys now (`None` when leaving the session): focus-out to
    /// the pane last told, then focus-in to the new one. A pane `routable`
    /// refuses hears nothing, and a focus-in it missed is offered again on
    /// the next call.
    pub(super) fn owed(
        &mut self,
        keyboard: Option<&ResourceId>,
        routable: impl Fn(&ResourceId) -> bool,
    ) -> Vec<(ResourceId, FocusEvent)> {
        let wanted = keyboard.filter(|_| self.outer);
        if self.told.as_ref() == wanted {
            return Vec::new();
        }
        let mut owed = Vec::with_capacity(2);
        if let Some(old) = self.told.take()
            && routable(&old)
        {
            owed.push((old, FocusEvent::Lost));
        }
        if let Some(new) = wanted
            && routable(new)
        {
            self.told = Some(new.clone());
            owed.push((new.clone(), FocusEvent::Gained));
        }
        owed
    }
}

/// Client-local pane focus and the session names this client has attached.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct FocusHistory {
    previous: Option<ResourceId>,
    /// The pane focused in the tab this client last switched away from. A
    /// pane names its tab however tabs are reordered.
    // ponytail: only tab switches record it; a jump made by `last-pane` or
    // a picker does not. Record at the focus transition if that matters.
    left_window: Option<ResourceId>,
    /// Attached session names, oldest first. Not persisted.
    sessions: Vec<String>,
}

impl FocusHistory {
    /// Seed history in focused dispatcher tests.
    #[cfg(test)]
    pub(super) const fn with_previous(previous: ResourceId) -> Self {
        Self {
            previous: Some(previous),
            left_window: None,
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

    /// Remember the tab being left by the pane focused in it.
    pub(super) fn leave_window(&mut self, focus: Option<ResourceId>) {
        self.left_window = focus;
    }

    /// The pane that names the tab last left, for `last-window`.
    pub(super) const fn left_window(&self) -> Option<&ResourceId> {
        self.left_window.as_ref()
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
        workspace: &ArrangementDoc,
    ) -> Option<ResourceId> {
        self.repair(current, workspace);
        self.previous.clone()
    }

    /// Drop history when its pane closed/disappeared or equals current focus.
    pub(super) fn repair(&mut self, current: Option<&ResourceId>, workspace: &ArrangementDoc) {
        let valid = self.previous.as_ref().is_some_and(|previous| {
            Some(previous) != current
                && workspace.groups.iter().any(|window| {
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
        let mut workspace = ArrangementDoc::single(tid(1));
        workspace.add_group("2".to_owned(), tid(2));
        workspace.select(0);
        let mut current = Some(tid(1));
        let mut history = FocusHistory::default();

        history.transition(&mut current, Some(tid(2)));
        assert_eq!(history.target(current.as_ref(), &workspace), Some(tid(1)));
        history.transition(&mut current, Some(tid(1)));
        assert_eq!(history.target(current.as_ref(), &workspace), Some(tid(2)));

        workspace.groups.pop();
        history.repair(current.as_ref(), &workspace);
        assert_eq!(history.previous(), None);
    }

    /// Each move tells the pane left and the pane reached exactly once; the
    /// outer window and leaving the session count as moves; a pane with no
    /// route yet is asked again once it has one.
    #[test]
    fn focus_reports_follow_every_move_once() {
        use FocusEvent::{Gained, Lost};
        let any = |_: &ResourceId| true;
        let mut reports = FocusReports::default();

        assert_eq!(reports.owed(Some(&tid(1)), any), vec![(tid(1), Gained)]);
        assert_eq!(reports.owed(Some(&tid(1)), any), vec![]);
        // Pane, tab or session switch: whatever moved the keyboard.
        assert_eq!(
            reports.owed(Some(&tid(2)), any),
            vec![(tid(1), Lost), (tid(2), Gained)]
        );
        // The outer window loses focus, then gets it back.
        reports.set_outer(false);
        assert_eq!(reports.owed(Some(&tid(2)), any), vec![(tid(2), Lost)]);
        assert_eq!(reports.owed(Some(&tid(3)), any), vec![]);
        reports.set_outer(true);
        assert_eq!(reports.owed(Some(&tid(3)), any), vec![(tid(3), Gained)]);
        // Leaving the session.
        assert_eq!(reports.owed(None, any), vec![(tid(3), Lost)]);
        assert_eq!(reports.owed(None, any), vec![]);

        // No stream yet: nothing now, focus-in once it can be carried.
        assert_eq!(reports.owed(Some(&tid(4)), |_| false), vec![]);
        assert_eq!(reports.owed(Some(&tid(4)), any), vec![(tid(4), Gained)]);
    }
}
