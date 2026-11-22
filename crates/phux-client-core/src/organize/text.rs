//! Plans and journals as lines for people.

use super::journal::Journal;
use super::plan::{JudgeKind, KeptReason, Plan};

/// `plan` as lines for people: creates, moves, renames, then the
/// terminals left in place because the judge was unsure.
#[must_use]
pub fn plan_lines(plan: &Plan) -> Vec<String> {
    let mut lines = Vec::new();
    for new in &plan.creates {
        lines.push(format!("create  {}  ({})", new.name, new.project_id));
    }
    for step in &plan.moves {
        lines.push(format!(
            "move    {}  {} -> {}  ({:.0}%)",
            step.label,
            step.from_name,
            step.to_name,
            step.confidence * 100.0
        ));
    }
    for rename in &plan.renames {
        lines.push(format!("rename  {} -> {}", rename.from, rename.to));
    }
    for kept in plan
        .kept
        .iter()
        .filter(|kept| kept.reason == KeptReason::Unsure)
    {
        lines.push(format!(
            "unsure  {}  {} -> {}?  ({:.0}%, left in place)",
            kept.label,
            kept.session,
            kept.suggested.as_deref().unwrap_or("?"),
            kept.confidence * 100.0
        ));
    }
    lines
}

/// `3 panes moved`, `1 session created`.
#[must_use]
pub fn count(n: usize, noun: &str, verb: &str) -> String {
    let plural = if n == 1 { "" } else { "s" };
    format!("{n} {noun}{plural} {verb}")
}

/// One short line for a status bar or toast: what `journal` did, plus
/// `failures` failed steps.
#[must_use]
pub fn journal_summary(judge: JudgeKind, journal: &Journal, failures: usize) -> String {
    let mut parts = vec![count(journal.moved.len(), "pane", "moved")];
    if !journal.created.is_empty() {
        parts.push(count(journal.created.len(), "session", "created"));
    }
    if !journal.renamed.is_empty() {
        parts.push(count(journal.renamed.len(), "session", "renamed"));
    }
    if failures > 0 {
        parts.push(count(failures, "step", "failed"));
    }
    format!("{} ({}).", parts.join(", "), judge.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::organize::journal::CreatedSession;
    use crate::organize::testkit::*;

    #[test]
    fn summaries_count_and_pluralize() {
        let mut journal = Journal::new();
        assert_eq!(
            journal_summary(JudgeKind::Jev, &journal, 0),
            "0 panes moved (jev)."
        );
        journal.created.push(CreatedSession {
            key: skey(1),
            name: "phux".into(),
        });
        assert_eq!(
            journal_summary(JudgeKind::Heuristic, &journal, 1),
            "0 panes moved, 1 session created, 1 step failed (heuristic)."
        );
    }
}
