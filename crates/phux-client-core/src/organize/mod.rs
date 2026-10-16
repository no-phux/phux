//! The organize planner: which session each terminal belongs in
//! (ADR-0174).
//!
//! Pure data in, a [`Plan`] out. The caller gathers [`SessionFacts`] and
//! [`PaneFacts`] from the organization records and the snapshot
//! ([`facts`]); [`candidates()`] lists the destinations; a judge picks one
//! per terminal ([`jev_request`] / [`read_jev_answers`] for Jev,
//! [`heuristic_verdicts`] without one); and [`plan()`] turns the verdicts
//! into session creates, membership moves, and session renames. Code keeps
//! the decision: a verdict below the confidence bar, or one that would pull
//! a terminal away from the session of its own repository, moves nothing.
//! Applying a plan yields a [`Journal`]; [`text`] renders both for people.
//!
//! Evidence is metadata only: names, titles, working directories, agent
//! providers, and project identities. Terminal contents never enter it.

pub mod candidates;
pub mod facts;
pub mod heuristic;
pub mod jev;
pub mod journal;
pub mod plan;
pub mod text;

pub use candidates::{
    Candidate, Candidates, Destination, MAX_OPTIONS, NewSession, candidates, generated_name,
};
pub use facts::{Facts, PaneFacts, Scope, SessionFacts, in_scope, scrub};
pub use heuristic::heuristic_verdicts;
pub use jev::{JEV_MODEL, JevAnswerError, QUESTIONS_PER_REQUEST, jev_request, read_jev_answers};
pub use journal::{CreatedSession, JOURNAL_SCHEMA, Journal, MovedPane};
pub use plan::{
    DEFAULT_MIN_CONFIDENCE, JudgeKind, Kept, KeptReason, Plan, PlanInput, PlannedMove,
    PlannedRename, Verdict, plan,
};
pub use text::{count, journal_summary, plan_lines};

#[cfg(test)]
mod testkit {
    use phux_protocol::ids::{ResourceKey, SessionKey};

    use super::*;

    pub(super) fn skey(n: u8) -> SessionKey {
        SessionKey::new([n; 16]).expect("non-zero")
    }

    pub(super) fn rkey(n: u8) -> ResourceKey {
        ResourceKey::new([n; 16]).expect("non-zero")
    }

    pub(super) fn session(n: u8, name: &str, project: Option<&str>) -> SessionFacts {
        SessionFacts {
            key: skey(n),
            name: name.to_owned(),
            intake: false,
            project_id: project.map(|p| format!("github.com/o/{p}")),
            project_name: project.map(str::to_owned),
            checkout: project.map(|p| format!("/src/{p}")),
        }
    }

    pub(super) fn pane(n: u8, session: u8, project: Option<&str>) -> PaneFacts {
        PaneFacts {
            key: rkey(n),
            label: format!("@{n}"),
            session: skey(session),
            title: None,
            cwd: project.map(|p| format!("/src/{p}/crates")),
            agent: None,
            status: "running".to_owned(),
            project_id: project.map(|p| format!("github.com/o/{p}")),
            project_name: project.map(str::to_owned),
            checkout: project.map(|p| format!("/src/{p}")),
        }
    }

    pub(super) fn intake(n: u8) -> SessionFacts {
        SessionFacts {
            intake: true,
            ..session(n, "intake", None)
        }
    }

    pub(super) fn run(sessions: &[SessionFacts], panes: &[PaneFacts], scope: Scope) -> Plan {
        let candidates = candidates(sessions, panes, scope);
        let verdicts = heuristic_verdicts(panes, scope, &candidates);
        plan(
            PlanInput {
                panes,
                scope,
                candidates: &candidates,
                judge: JudgeKind::Heuristic,
                min_confidence: DEFAULT_MIN_CONFIDENCE,
            },
            &verdicts,
        )
    }
}
