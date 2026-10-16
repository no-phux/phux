//! The judge without a model: repository matching, then checkout
//! containment.

use phux_protocol::ids::SessionKey;

use super::candidates::{Candidates, Destination};
use super::facts::{PaneFacts, Scope, SessionFacts, in_scope};
use super::plan::Verdict;

/// Verdicts without a model: a terminal goes to the session carrying its
/// repository (certain), else to the one whose checkout holds its working
/// directory (likely), else stays.
#[must_use]
pub fn heuristic_verdicts(
    panes: &[PaneFacts],
    scope: Scope,
    candidates: &Candidates,
) -> Vec<Verdict> {
    let sessions = candidates.sessions.as_slice();
    in_scope(panes, scope)
        .into_iter()
        .map(|pane| {
            let (destination, confidence) = heuristic_destination(sessions, pane, candidates);
            Verdict {
                pane: pane.key,
                destination,
                confidence,
            }
        })
        .collect()
}

fn heuristic_destination(
    sessions: &[SessionFacts],
    pane: &PaneFacts,
    candidates: &Candidates,
) -> (Destination, f64) {
    if let Some(destination) = repository_home(sessions, pane, candidates) {
        return (destination, 1.0);
    }
    if let Some(session) = checkout_home(sessions, pane) {
        return (Destination::Existing(session), 0.8);
    }
    (Destination::Stay, 0.0)
}

/// The session (or proposed session) carrying `pane`'s repository; its own
/// session when that one carries it.
fn repository_home(
    sessions: &[SessionFacts],
    pane: &PaneFacts,
    candidates: &Candidates,
) -> Option<Destination> {
    let id = pane.project_id.as_deref()?;
    let homes: Vec<&SessionFacts> = sessions
        .iter()
        .filter(|session| !session.intake && session.project_id.as_deref() == Some(id))
        .collect();
    if let Some(own) = homes.iter().find(|session| session.key == pane.session) {
        return Some(Destination::Existing(own.key));
    }
    if let Some(first) = homes.first() {
        return Some(Destination::Existing(first.key));
    }
    candidates
        .new_sessions
        .iter()
        .position(|new| new.project_id == id)
        .map(Destination::New)
}

fn checkout_home(sessions: &[SessionFacts], pane: &PaneFacts) -> Option<SessionKey> {
    let cwd = pane.cwd.as_deref()?;
    sessions
        .iter()
        .filter(|session| !session.intake)
        .filter_map(|session| Some((session, session.checkout.as_deref()?)))
        .filter(|(_, checkout)| within(cwd, checkout))
        .max_by_key(|(_, checkout)| checkout.len())
        .map(|(session, _)| session.key)
}

/// Whether `path` is `dir` or below it.
fn within(path: &str, dir: &str) -> bool {
    let dir = dir.trim_end_matches('/');
    !dir.is_empty()
        && path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::organize::plan::KeptReason;
    use crate::organize::testkit::*;

    /// Intake terminals go to their repository's session; one with no
    /// repository stays; Intake is never a destination.
    #[test]
    fn intake_terminals_join_their_repository() {
        let sessions = [intake(1), session(2, "phux", Some("phux"))];
        let panes = [pane(10, 1, Some("phux")), pane(11, 1, None)];
        let plan = run(&sessions, &panes, Scope::All);
        assert_eq!(plan.moves.len(), 1, "{plan:?}");
        assert_eq!(plan.moves[0].to, Some(skey(2)));
        assert_eq!(plan.kept.len(), 1);
        assert_eq!(plan.kept[0].reason, KeptReason::InPlace);
        assert!(plan.creates.is_empty());
    }

    #[test]
    fn within_respects_path_boundaries() {
        assert!(within("/src/phux", "/src/phux/"));
        assert!(within("/src/phux/a", "/src/phux"));
        assert!(!within("/src/phuxx", "/src/phux"));
        assert!(!within("/a", ""));
    }
}
