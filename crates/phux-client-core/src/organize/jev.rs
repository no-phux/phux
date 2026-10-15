//! The Jev wire format: one Choice question per in-scope terminal over the
//! candidates plus "leave where it is", and the verdicts its answers name.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Value, json};

use super::candidates::{Candidates, Destination, STAY_OPTION};
use super::facts::{PaneFacts, Scope, in_scope};
use super::plan::Verdict;

/// The Jev model alias every request names.
pub const JEV_MODEL: &str = "jev-latest";

/// Most questions one Jev request carries.
pub const QUESTIONS_PER_REQUEST: usize = 40;

/// The Jev request bodies that ask where each in-scope terminal belongs,
/// [`QUESTIONS_PER_REQUEST`] questions each. Question ids are `p<index>`
/// into [`in_scope`]'s order.
#[must_use]
pub fn jev_request(panes: &[PaneFacts], scope: Scope, candidates: &Candidates) -> Vec<Value> {
    let scoped = in_scope(panes, scope);
    scoped
        .chunks(QUESTIONS_PER_REQUEST)
        .enumerate()
        .map(|(chunk, panes)| {
            let questions: serde_json::Map<String, Value> = panes
                .iter()
                .enumerate()
                .map(|(offset, pane)| {
                    let id = format!("p{}", chunk * QUESTIONS_PER_REQUEST + offset);
                    (id, choice_question(pane, candidates))
                })
                .collect();
            json!({
                "model": JEV_MODEL,
                "state": {
                    "application": "phux, a terminal multiplexer",
                    "goal": "Group terminals into sessions so each session holds one project or line of work.",
                },
                "questions": questions,
            })
        })
        .collect()
}

fn choice_question(pane: &PaneFacts, candidates: &Candidates) -> Value {
    let mut criteria: serde_json::Map<String, Value> = candidates
        .options
        .iter()
        .map(|candidate| (candidate.option.clone(), candidate.criterion(pane.key)))
        .collect();
    criteria.insert(
        STAY_OPTION.to_owned(),
        json!("No session fits better than the one it is in now."),
    );
    let mut terminal = pane.evidence();
    let current = candidates
        .sessions
        .iter()
        .find(|session| session.key == pane.session);
    if let (Some(map), Some(current)) = (terminal.as_object_mut(), current) {
        map.insert("current_session".into(), json!(current.name));
    }
    json!({
        "type": "choice",
        "instructions": {
            "terminal": terminal,
            "question": "Which session should `terminal` belong to? Judge by its repository and working directory first, then by what its title or agent says it works on: a terminal whose title names a project or task belongs with that project's session. A new session fits only a terminal in that new session's repository. Choose its current session, or leave it where it is, when nothing fits better.",
        },
        "criteria": criteria,
    })
}

/// One Choice answer as the Jev API returns it.
#[derive(Debug, Clone, Deserialize)]
struct ChoiceAnswer {
    choice: String,
    #[serde(default)]
    confidence: f64,
}

/// A Jev response body that did not carry the expected answers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unreadable Jev answer: {0}")]
pub struct JevAnswerError(pub String);

/// The verdicts in Jev response bodies answering [`jev_request`]. A
/// question with no answer, or an answer naming no option, yields no
/// verdict: that terminal stays.
///
/// # Errors
///
/// A body without an `answers` object.
pub fn read_jev_answers(
    panes: &[PaneFacts],
    scope: Scope,
    candidates: &Candidates,
    responses: &[Value],
) -> Result<Vec<Verdict>, JevAnswerError> {
    let mut answers: HashMap<String, ChoiceAnswer> = HashMap::new();
    for response in responses {
        let map = response
            .get("answers")
            .and_then(Value::as_object)
            .ok_or_else(|| JevAnswerError("no `answers` object".to_owned()))?;
        for (id, answer) in map {
            if let Ok(answer) = serde_json::from_value::<ChoiceAnswer>(answer.clone()) {
                answers.insert(id.clone(), answer);
            }
        }
    }
    let verdicts = in_scope(panes, scope)
        .into_iter()
        .enumerate()
        .filter_map(|(index, pane)| {
            let answer = answers.get(&format!("p{index}"))?;
            Some(Verdict {
                pane: pane.key,
                destination: destination_of(candidates, &answer.choice)?,
                confidence: answer.confidence,
            })
        })
        .collect();
    Ok(verdicts)
}

pub(super) fn destination_of(candidates: &Candidates, option: &str) -> Option<Destination> {
    if option == STAY_OPTION {
        return Some(Destination::Stay);
    }
    candidates
        .by_option(option)
        .map(|candidate| candidate.destination.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::organize::candidates::candidates;
    use crate::organize::testkit::*;

    /// The request asks one Choice per in-scope terminal with every option
    /// plus "leave where it is", and the answers map back to destinations.
    #[test]
    fn jev_round_trip() {
        let sessions = [intake(1), session(2, "phux", Some("phux"))];
        let panes = [pane(10, 1, None)];
        let candidates = candidates(&sessions, &panes, Scope::All);
        let requests = jev_request(&panes, Scope::All, &candidates);
        assert_eq!(requests.len(), 1);
        let criteria = &requests[0]["questions"]["p0"]["criteria"];
        assert!(criteria.get("phux").is_some(), "{criteria}");
        assert!(criteria.get(STAY_OPTION).is_some());
        assert!(criteria.get("intake").is_none());
        let response = json!({"answers": {"p0": {
            "type": "choice", "choice": "phux",
            "probabilities": {"phux": 0.9, STAY_OPTION: 0.1}, "confidence": 0.85}}});
        let verdicts =
            read_jev_answers(&panes, Scope::All, &candidates, &[response]).expect("answers");
        assert_eq!(verdicts[0].destination, Destination::Existing(skey(2)));
        assert!((verdicts[0].confidence - 0.85).abs() < f64::EPSILON);
        assert!(read_jev_answers(&panes, Scope::All, &candidates, &[json!({})]).is_err());
    }
}
