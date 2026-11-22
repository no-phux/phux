//! Verdicts become a plan here, under rules in code: the confidence bar,
//! and a terminal in its own repository's session never moves.

use std::collections::{BTreeSet, HashMap};

use phux_protocol::ids::{ResourceKey, SessionKey};
use serde::{Deserialize, Serialize};

use super::candidates::{Candidates, Destination, NewSession};
use super::facts::{PaneFacts, Scope, SessionFacts, in_scope};

/// Default confidence a verdict needs before it moves anything.
pub const DEFAULT_MIN_CONFIDENCE: f64 = 0.6;

/// Which judge produced the verdicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgeKind {
    /// `TypeSafe`'s Jev model.
    Jev,
    /// Repository and directory matching, without a model.
    Heuristic,
}

impl JudgeKind {
    /// The judge's name as reports print it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Jev => "jev",
            Self::Heuristic => "heuristic",
        }
    }
}

/// One judge's answer for one terminal.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    /// The terminal.
    pub pane: ResourceKey,
    /// Where it should go.
    pub destination: Destination,
    /// How sure the judge is, 0 to 1.
    pub confidence: f64,
}

/// One membership move in a plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedMove {
    /// The terminal.
    pub pane: ResourceKey,
    /// How a person addresses it.
    pub label: String,
    /// The session it leaves.
    pub from: SessionKey,
    /// That session's name.
    pub from_name: String,
    /// The session it joins: an existing key, or `None` for a new session.
    pub to: Option<SessionKey>,
    /// That session's name.
    pub to_name: String,
    /// The judge's confidence.
    pub confidence: f64,
}

/// A terminal the plan leaves in place, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Kept {
    /// The terminal.
    pub pane: ResourceKey,
    /// How a person addresses it.
    pub label: String,
    /// Its session's name.
    pub session: String,
    /// Why it stays.
    pub reason: KeptReason,
    /// Where the judge would have sent it, when that was elsewhere.
    pub suggested: Option<String>,
    /// The judge's confidence.
    pub confidence: f64,
}

/// Why a terminal stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeptReason {
    /// It is already where it belongs.
    InPlace,
    /// The judge was not sure enough.
    Unsure,
    /// It is in its own repository's session; the judge cannot pull it out.
    WithItsRepository,
    /// The judge gave no answer for it.
    NoVerdict,
}

/// A session the plan renames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedRename {
    /// The session.
    pub session: SessionKey,
    /// Its name now.
    pub from: String,
    /// Its new name.
    pub to: String,
}

/// What one organize run will do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    /// Which judge decided.
    pub judge: JudgeKind,
    /// Sessions to create first.
    pub creates: Vec<NewSession>,
    /// Membership moves.
    pub moves: Vec<PlannedMove>,
    /// Sessions to rename after the moves.
    pub renames: Vec<PlannedRename>,
    /// Terminals left in place.
    pub kept: Vec<Kept>,
}

impl Plan {
    /// Whether applying the plan changes nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.creates.is_empty() && self.moves.is_empty() && self.renames.is_empty()
    }
}

/// Everything [`plan`] reads.
#[derive(Debug, Clone, Copy)]
pub struct PlanInput<'a> {
    /// Every terminal.
    pub panes: &'a [PaneFacts],
    /// Which terminals may move.
    pub scope: Scope,
    /// The destinations the judge chose from.
    pub candidates: &'a Candidates,
    /// Which judge produced `verdicts`.
    pub judge: JudgeKind,
    /// Confidence a verdict needs to move a terminal.
    pub min_confidence: f64,
}

/// Turn verdicts into a plan: gate each one, create only the new sessions a
/// move lands in, and carry the candidates' adoption renames.
#[must_use]
pub fn plan(input: PlanInput<'_>, verdicts: &[Verdict]) -> Plan {
    let names: HashMap<SessionKey, &SessionFacts> = input
        .candidates
        .sessions
        .iter()
        .map(|s| (s.key, s))
        .collect();
    let by_pane: HashMap<ResourceKey, &Verdict> = verdicts.iter().map(|v| (v.pane, v)).collect();
    let mut moves = Vec::new();
    let mut kept = Vec::new();
    let mut used_new: BTreeSet<usize> = BTreeSet::new();
    for pane in in_scope(input.panes, input.scope) {
        let from_name = names
            .get(&pane.session)
            .map_or_else(String::new, |s| s.name.clone());
        match gate(&input, &names, pane, by_pane.get(&pane.key).copied()) {
            Gate::Move(destination, confidence) => {
                let (to, to_name) = resolve(&input, &names, &destination);
                if let Destination::New(index) = destination {
                    used_new.insert(index);
                }
                moves.push(PlannedMove {
                    pane: pane.key,
                    label: pane.label.clone(),
                    from: pane.session,
                    from_name,
                    to,
                    to_name,
                    confidence,
                });
            }
            Gate::Keep(reason, suggested, confidence) => kept.push(Kept {
                pane: pane.key,
                label: pane.label.clone(),
                session: from_name,
                reason,
                suggested: suggested.map(|d| resolve(&input, &names, &d).1),
                confidence,
            }),
        }
    }
    let creates = used_new
        .iter()
        .filter_map(|index| input.candidates.new_sessions.get(*index).cloned())
        .collect();
    let renames = input.candidates.renames.clone();
    Plan {
        judge: input.judge,
        creates,
        moves,
        renames,
        kept,
    }
}

enum Gate {
    Move(Destination, f64),
    Keep(KeptReason, Option<Destination>, f64),
}

fn gate(
    input: &PlanInput<'_>,
    names: &HashMap<SessionKey, &SessionFacts>,
    pane: &PaneFacts,
    verdict: Option<&Verdict>,
) -> Gate {
    let Some(verdict) = verdict else {
        return Gate::Keep(KeptReason::NoVerdict, None, 0.0);
    };
    let confidence = verdict.confidence;
    let stays = match &verdict.destination {
        Destination::Stay => true,
        Destination::Existing(key) => *key == pane.session,
        Destination::New(_) => false,
    };
    if stays {
        return Gate::Keep(KeptReason::InPlace, None, confidence);
    }
    let suggested = Some(verdict.destination.clone());
    if with_its_repository(names, pane) {
        return Gate::Keep(KeptReason::WithItsRepository, suggested, confidence);
    }
    if confidence < input.min_confidence {
        return Gate::Keep(KeptReason::Unsure, suggested, confidence);
    }
    Gate::Move(verdict.destination.clone(), confidence)
}

/// Whether `pane` already sits in a session carrying its own repository.
fn with_its_repository(names: &HashMap<SessionKey, &SessionFacts>, pane: &PaneFacts) -> bool {
    let Some(id) = pane.project_id.as_deref() else {
        return false;
    };
    names
        .get(&pane.session)
        .is_some_and(|session| !session.intake && session.project_id.as_deref() == Some(id))
}

fn resolve(
    input: &PlanInput<'_>,
    names: &HashMap<SessionKey, &SessionFacts>,
    destination: &Destination,
) -> (Option<SessionKey>, String) {
    match destination {
        Destination::Existing(key) => (
            Some(*key),
            names.get(key).map_or_else(String::new, |s| s.name.clone()),
        ),
        Destination::New(index) => (
            None,
            input
                .candidates
                .new_sessions
                .get(*index)
                .map_or_else(String::new, |new| new.name.clone()),
        ),
        Destination::Stay => (None, String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::organize::candidates::candidates;
    use crate::organize::testkit::*;

    /// Session scope moves only that session's members.
    #[test]
    fn session_scope_moves_only_its_members() {
        let sessions = [
            session(1, "misc", None),
            session(2, "phux", Some("phux")),
            session(3, "other", None),
        ];
        let panes = [pane(10, 1, Some("phux")), pane(11, 3, Some("phux"))];
        let plan = run(&sessions, &panes, Scope::Session(skey(1)));
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(plan.moves[0].pane, rkey(10));
    }

    /// A terminal in its own repository's session never leaves it, however
    /// sure the judge is; an unsure verdict moves nothing.
    #[test]
    fn code_vetoes_and_gates_the_judge() {
        let sessions = [
            session(1, "phux", Some("phux")),
            session(2, "web", Some("web")),
        ];
        let panes = [pane(10, 1, Some("phux")), pane(11, 2, None)];
        let candidates = candidates(&sessions, &panes, Scope::All);
        let verdicts = [
            Verdict {
                pane: rkey(10),
                destination: Destination::Existing(skey(2)),
                confidence: 0.99,
            },
            Verdict {
                pane: rkey(11),
                destination: Destination::Existing(skey(1)),
                confidence: 0.3,
            },
        ];
        let plan = plan(
            PlanInput {
                panes: &panes,
                scope: Scope::All,
                candidates: &candidates,
                judge: JudgeKind::Jev,
                min_confidence: DEFAULT_MIN_CONFIDENCE,
            },
            &verdicts,
        );
        assert!(plan.moves.is_empty(), "{plan:?}");
        assert_eq!(plan.kept[0].reason, KeptReason::WithItsRepository);
        assert_eq!(plan.kept[1].reason, KeptReason::Unsure);
        assert_eq!(plan.kept[1].suggested.as_deref(), Some("phux"));
    }
}
