//! Organization reads and outcomes as the window shows them: arrangement
//! group names per terminal, operation outcomes, and kill-session reports.

use std::collections::HashMap;

use phux_client_core::layout::leaves;
use phux_client_core::org_ops::{
    KillReport, OrgOutcome, SessionOutcome, TargetOutcome, refusal_sentence,
};
use phux_client_runtime::control::OrganizationReplica;
use phux_protocol::ResourceId;
use phux_protocol::ids::ResourceKey;

/// The arrangement group that shows each terminal, by name, for every
/// session whose arrangement the replica holds.
#[must_use]
pub fn group_names(replica: &OrganizationReplica) -> HashMap<ResourceId, String> {
    let mut names = HashMap::new();
    for session in replica.sessions() {
        let Some((_, _, document, _)) = replica.arrangement_document(&session.key) else {
            continue;
        };
        for group in &document.groups {
            let Some(tree) = group.state.tree.as_ref() else {
                continue;
            };
            for terminal in leaves(tree) {
                names.entry(terminal).or_insert_with(|| group.name.clone());
            }
        }
    }
    names
}

/// A sentence for an organization operation that did not take effect.
#[must_use]
pub fn outcome_sentence(outcome: &OrgOutcome) -> String {
    match outcome {
        OrgOutcome::Refused(refusal) => refusal_sentence(refusal),
        OrgOutcome::Error { message, .. } => message.clone(),
        OrgOutcome::Unknown { reason } => {
            format!("the outcome is unknown ({reason}); check the sidebar before retrying")
        }
        OrgOutcome::NotSent { reason } => format!("not sent: {reason}"),
        _ => "it took effect".to_owned(),
    }
}

/// What a finished kill tells the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillSummary {
    /// Whether the session and every member are gone.
    pub complete: bool,
    /// Toast title.
    pub title: String,
    /// Toast body: one clause per target that did not end cleanly, else a count.
    pub body: String,
}

/// Summarize `report` for session `name`. `labels` names members by key;
/// a member without one is named by its key.
#[must_use]
pub fn kill_summary(
    name: &str,
    report: &KillReport,
    labels: &HashMap<ResourceKey, String>,
) -> KillSummary {
    let ended = report
        .targets
        .iter()
        .filter(|(_, outcome)| outcome.is_gone())
        .count();
    let mut clauses: Vec<String> = report
        .targets
        .iter()
        .filter(|(_, outcome)| !outcome.is_gone())
        .map(|(key, outcome)| {
            let label = labels.get(key).cloned().unwrap_or_else(|| key.to_string());
            format!("{label}: {}", target_sentence(outcome))
        })
        .collect();
    if let Some(session) = session_sentence(&report.session_outcome) {
        clauses.push(session);
    }
    let complete = report.complete() && report.targets.iter().all(|(_, o)| o.is_gone());
    let count = format!(
        "{ended} {} ended",
        if ended == 1 { "terminal" } else { "terminals" }
    );
    if complete {
        KillSummary {
            complete,
            title: format!("Killed {name}"),
            body: count,
        }
    } else {
        clauses.insert(0, count);
        KillSummary {
            complete,
            title: format!("Kill of {name} did not finish"),
            body: clauses.join("; "),
        }
    }
}

fn target_sentence(outcome: &TargetOutcome) -> String {
    match outcome {
        TargetOutcome::Removed => "ended".to_owned(),
        TargetOutcome::AlreadyGone => "already gone".to_owned(),
        TargetOutcome::Refused(refusal) => format!("refused, {}", refusal_sentence(refusal)),
        TargetOutcome::Unknown(reason) => format!("outcome unknown ({reason})"),
        TargetOutcome::TimedOut => "still running; it did not stop in time".to_owned(),
        TargetOutcome::NoLiveTarget => "runs on another server; not ended from here".to_owned(),
    }
}

fn session_sentence(outcome: &SessionOutcome) -> Option<String> {
    match outcome {
        SessionOutcome::Deleted | SessionOutcome::AlreadyGone => None,
        SessionOutcome::Kept => Some("the session was kept because a member remains".to_owned()),
        SessionOutcome::Refused(refusal) => Some(format!(
            "the session delete was refused, {}",
            refusal_sentence(refusal)
        )),
        SessionOutcome::Unknown(reason) => Some(format!(
            "the session delete's outcome is unknown ({reason})"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::ids::SessionKey;
    use phux_protocol::wire::frame::Refusal;

    fn key(byte: u8) -> ResourceKey {
        ResourceKey::new([byte; 16]).expect("non-zero key")
    }

    fn report(targets: Vec<(ResourceKey, TargetOutcome)>, session: SessionOutcome) -> KillReport {
        KillReport {
            session: SessionKey::new([9; 16]).expect("non-zero key"),
            targets,
            session_outcome: session,
        }
    }

    #[test]
    fn a_complete_kill_reports_a_count() {
        let summary = kill_summary(
            "work",
            &report(
                vec![
                    (key(1), TargetOutcome::Removed),
                    (key(2), TargetOutcome::AlreadyGone),
                ],
                SessionOutcome::Deleted,
            ),
            &HashMap::new(),
        );
        assert!(summary.complete);
        assert_eq!(summary.title, "Killed work");
        assert_eq!(summary.body, "2 terminals ended");
    }

    #[test]
    fn a_partial_kill_names_each_target_that_survived() {
        let labels = HashMap::from([(key(2), "server".to_owned())]);
        let summary = kill_summary(
            "work",
            &report(
                vec![
                    (key(1), TargetOutcome::Removed),
                    (key(2), TargetOutcome::TimedOut),
                    (key(3), TargetOutcome::Refused(Refusal::NotFound)),
                ],
                SessionOutcome::Kept,
            ),
            &labels,
        );
        assert!(!summary.complete);
        assert_eq!(summary.title, "Kill of work did not finish");
        assert!(
            summary
                .body
                .starts_with("1 terminal ended; server: still running")
        );
        assert!(
            summary
                .body
                .contains(&format!("{}: refused, no such record", key(3)))
        );
        assert!(
            summary
                .body
                .ends_with("the session was kept because a member remains")
        );
    }
}
