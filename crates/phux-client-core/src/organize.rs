//! The organize planner: which session each terminal belongs in
//! (ADR-0170).
//!
//! Pure data in, a [`Plan`] out. The caller gathers [`SessionFacts`] and
//! [`PaneFacts`] from the organization records and the snapshot, a judge
//! picks a destination per terminal ([`jev_request`] / [`read_jev_answers`]
//! for Jev, [`heuristic_verdicts`] without one), and [`plan`] turns the
//! verdicts into session creates, membership moves, and session renames.
//! Code keeps the decision: a verdict below the confidence bar, or one that
//! would pull a terminal away from the session of its own repository, moves
//! nothing.
//!
//! Evidence is metadata only: names, titles, working directories, agent
//! providers, and project identities. Terminal contents never enter it.

use std::collections::{BTreeSet, HashMap, HashSet};

use phux_protocol::ids::{ResourceKey, SessionKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The Jev model alias every request names.
pub const JEV_MODEL: &str = "jev-latest";

/// Most options one Choice question may carry (the API allows 255).
pub const MAX_OPTIONS: usize = 255;

/// Most questions one Jev request carries.
pub const QUESTIONS_PER_REQUEST: usize = 40;

/// Default confidence a verdict needs before it moves anything.
pub const DEFAULT_MIN_CONFIDENCE: f64 = 0.6;

/// Member summaries shown per session in a Choice option.
const MEMBERS_PER_OPTION: usize = 6;

/// The option that leaves a terminal where it is when no session fits.
const STAY_OPTION: &str = "leave where it is";

/// One session as the planner sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFacts {
    /// Its record key.
    pub key: SessionKey,
    /// Its name.
    pub name: String,
    /// The designated Intake session: never a destination.
    pub intake: bool,
    /// Its project identity (`project.id`), when it carries one.
    pub project_id: Option<String>,
    /// Its project display name.
    pub project_name: Option<String>,
    /// Its project checkout root.
    pub checkout: Option<String>,
}

/// One terminal as the planner sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneFacts {
    /// Its record key.
    pub key: ResourceKey,
    /// How a person addresses it (`@3`, else `^ab23cd`).
    pub label: String,
    /// The session it belongs to now.
    pub session: SessionKey,
    /// Its name or title.
    pub title: Option<String>,
    /// Its working directory.
    pub cwd: Option<String>,
    /// The agent running there and its state (`claude blocked`).
    pub agent: Option<String>,
    /// Its state in words (`running`, `failed (exit 1)`).
    pub status: String,
    /// The project its working directory is in.
    pub project_id: Option<String>,
    /// That project's display name.
    pub project_name: Option<String>,
    /// That project's checkout root.
    pub checkout: Option<String>,
}

impl PaneFacts {
    /// The evidence one Jev question carries about this terminal.
    fn evidence(&self) -> Value {
        let mut map = serde_json::Map::new();
        map.insert("terminal".into(), json!(self.label));
        insert_some(&mut map, "title", self.title.as_deref());
        insert_some(&mut map, "working_directory", self.cwd.as_deref());
        insert_some(&mut map, "agent", self.agent.as_deref());
        insert_some(&mut map, "project", self.project_name.as_deref());
        insert_some(&mut map, "repository", self.project_id.as_deref());
        map.insert("status".into(), json!(self.status));
        Value::Object(map)
    }

    /// A one-line summary for a session's option description.
    fn summary(&self) -> String {
        let what = self
            .agent
            .as_deref()
            .or(self.title.as_deref())
            .unwrap_or("shell");
        self.cwd
            .as_deref()
            .map_or_else(|| what.to_owned(), |cwd| format!("{what} in {cwd}"))
    }
}

fn insert_some(map: &mut serde_json::Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        map.insert(key.to_owned(), json!(value));
    }
}

/// Which terminals the planner may move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every terminal on the server.
    All,
    /// Only this session's members; destinations are still every session.
    Session(SessionKey),
}

impl Scope {
    fn covers(self, pane: &PaneFacts) -> bool {
        match self {
            Self::All => true,
            Self::Session(key) => pane.session == key,
        }
    }
}

/// A session the plan creates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSession {
    /// Its name, unique among the sessions the planner saw.
    pub name: String,
    /// The project identity it carries (`project.id`).
    pub project_id: String,
    /// The project's checkout root, for its `project.checkout` attribute.
    pub checkout: Option<String>,
}

/// Where a verdict sends a terminal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Destination {
    /// An existing session.
    Existing(SessionKey),
    /// A session the plan creates, by its index in the candidate list's
    /// new sessions.
    New(usize),
    /// Nowhere: leave the terminal where it is.
    Stay,
}

/// One destination a judge may pick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The option text the judge sees and answers with.
    pub option: String,
    /// What the option means.
    pub destination: Destination,
    /// What the judge is told about the option, besides its members.
    pub description: Value,
    /// Its members' one-line summaries, by terminal.
    pub members: Vec<(ResourceKey, String)>,
}

impl Candidate {
    /// What the judge is told about this option when asking about `asked`:
    /// the description plus every member but `asked` itself, so a terminal
    /// does not count as evidence for its own session.
    fn criterion(&self, asked: ResourceKey) -> Value {
        let mut description = self.description.clone();
        let members: Vec<&str> = self
            .members
            .iter()
            .filter(|(key, _)| *key != asked)
            .take(MEMBERS_PER_OPTION)
            .map(|(_, summary)| summary.as_str())
            .collect();
        if let (Some(map), false) = (description.as_object_mut(), members.is_empty()) {
            map.insert("terminals".into(), json!(members));
        }
        description
    }
}

/// Every destination of one planning run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidates {
    /// Existing sessions, then proposed new ones.
    pub options: Vec<Candidate>,
    /// The sessions [`Destination::New`] indexes.
    pub new_sessions: Vec<NewSession>,
    /// Every session, generated-name ones adopted by their repository.
    pub sessions: Vec<SessionFacts>,
    /// The renames those adoptions make.
    pub renames: Vec<PlannedRename>,
}

impl Candidates {
    /// The candidate whose option text is `option`.
    #[must_use]
    pub fn by_option(&self, option: &str) -> Option<&Candidate> {
        self.options
            .iter()
            .find(|candidate| candidate.option == option)
    }
}

/// The destinations for `panes` in `scope`.
///
/// Every session but Intake, plus one new session per repository an
/// in-scope terminal works in that no session carries yet. A generated-name session whose terminals all work in
/// one repository no other session carries is adopted first: it takes the
/// repository's name and becomes its home.
#[must_use]
pub fn candidates(sessions: &[SessionFacts], panes: &[PaneFacts], scope: Scope) -> Candidates {
    let (adopted, renames) = adopt(sessions, panes, scope);
    let sessions = adopted.as_slice();
    let mut options: Vec<Candidate> = sessions
        .iter()
        .filter(|session| !session.intake)
        .map(|session| existing_candidate(session, panes))
        .collect();
    let mut taken: HashSet<String> = sessions.iter().map(|s| s.name.clone()).collect();
    let mut new_sessions = Vec::new();
    for pane in panes.iter().filter(|pane| scope.covers(pane)) {
        let Some(proposal) = proposed_session(sessions, &new_sessions, pane, &mut taken) else {
            continue;
        };
        options.push(new_candidate(&proposal, new_sessions.len()));
        new_sessions.push(proposal);
    }
    options.truncate(MAX_OPTIONS - 1);
    new_sessions.truncate(
        options
            .iter()
            .filter(|candidate| matches!(candidate.destination, Destination::New(_)))
            .count(),
    );
    Candidates {
        options,
        new_sessions,
        sessions: adopted,
        renames,
    }
}

/// `sessions` with each in-scope generated-name session adopted by the one
/// repository all its terminals work in, when no other session carries it
/// and no session holds its name; plus the renames that makes.
fn adopt(
    sessions: &[SessionFacts],
    panes: &[PaneFacts],
    scope: Scope,
) -> (Vec<SessionFacts>, Vec<PlannedRename>) {
    let mut out = sessions.to_vec();
    let mut renames = Vec::new();
    let mut taken: HashSet<String> = sessions.iter().map(|s| s.name.clone()).collect();
    for index in 0..out.len() {
        let Some(home) = adoptable(&out, index, panes, scope) else {
            continue;
        };
        if taken.contains(&home.name) {
            continue;
        }
        taken.insert(home.name.clone());
        let session = &mut out[index];
        renames.push(PlannedRename {
            session: session.key,
            from: std::mem::replace(&mut session.name, home.name.clone()),
            to: home.name.clone(),
        });
        session.project_id = Some(home.id);
        session.project_name = Some(home.name);
        session.checkout = home.checkout;
    }
    (out, renames)
}

/// The repository `sessions[index]` should be adopted by: an in-scope,
/// generated-name, non-Intake session whose members all work in one
/// repository it does not contradict and no other session carries.
fn adoptable(
    sessions: &[SessionFacts],
    index: usize,
    panes: &[PaneFacts],
    scope: Scope,
) -> Option<Home> {
    let session = &sessions[index];
    let in_scope = match scope {
        Scope::All => true,
        Scope::Session(key) => key == session.key,
    };
    if session.intake || !in_scope || !generated_name(&session.name) {
        return None;
    }
    let home = sole_project(panes.iter().filter(|pane| pane.session == session.key))?;
    let contradicts = session
        .project_id
        .as_deref()
        .is_some_and(|id| id != home.id);
    let carried_elsewhere = sessions.iter().any(|other| {
        other.key != session.key
            && !other.intake
            && other.project_id.as_deref() == Some(home.id.as_str())
    });
    (!contradicts && !carried_elsewhere).then_some(home)
}

/// The repository every one of `panes` works in.
struct Home {
    id: String,
    name: String,
    checkout: Option<String>,
}

fn sole_project<'a>(mut panes: impl Iterator<Item = &'a PaneFacts>) -> Option<Home> {
    let first = panes.next()?;
    let id = first.project_id.as_deref()?;
    if panes.any(|pane| pane.project_id.as_deref() != Some(id)) {
        return None;
    }
    Some(Home {
        id: id.to_owned(),
        name: first
            .project_name
            .clone()
            .unwrap_or_else(|| repo_name_of(id)),
        checkout: first.checkout.clone(),
    })
}

fn existing_candidate(session: &SessionFacts, panes: &[PaneFacts]) -> Candidate {
    let members: Vec<(ResourceKey, String)> = panes
        .iter()
        .filter(|pane| pane.session == session.key)
        .take(MEMBERS_PER_OPTION + 1)
        .map(|pane| (pane.key, pane.summary()))
        .collect();
    let mut description = serde_json::Map::new();
    description.insert("kind".into(), json!("existing session"));
    insert_some(&mut description, "project", session.project_name.as_deref());
    insert_some(
        &mut description,
        "repository",
        session.project_id.as_deref(),
    );
    insert_some(&mut description, "checkout", session.checkout.as_deref());
    Candidate {
        option: session.name.clone(),
        destination: Destination::Existing(session.key),
        description: Value::Object(description),
        members,
    }
}

fn new_candidate(proposal: &NewSession, index: usize) -> Candidate {
    Candidate {
        option: format!("{} (new session)", proposal.name),
        destination: Destination::New(index),
        description: json!({
            "kind": "new session for one repository",
            "repository": proposal.project_id,
            "checkout": proposal.checkout,
        }),
        members: Vec::new(),
    }
}

/// A new session for `pane`'s repository, when it has one no session or
/// earlier proposal carries.
fn proposed_session(
    sessions: &[SessionFacts],
    proposed: &[NewSession],
    pane: &PaneFacts,
    taken: &mut HashSet<String>,
) -> Option<NewSession> {
    let id = pane.project_id.as_deref()?;
    let carried = sessions
        .iter()
        .any(|session| !session.intake && session.project_id.as_deref() == Some(id))
        || proposed.iter().any(|new| new.project_id == id);
    if carried {
        return None;
    }
    let base = pane
        .project_name
        .clone()
        .unwrap_or_else(|| repo_name_of(id));
    let name = unique_name(&base, taken);
    taken.insert(name.clone());
    Some(NewSession {
        name,
        project_id: id.to_owned(),
        checkout: pane.checkout.clone(),
    })
}

/// The last path segment of a project id (`github.com/a/b` -> `b`).
fn repo_name_of(id: &str) -> String {
    id.rsplit(['/', ':'])
        .find(|segment| !segment.is_empty())
        .unwrap_or(id)
        .to_owned()
}

/// `base`, else `base-2`, `base-3`, ... whichever no session holds.
fn unique_name(base: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(base) {
        return base.to_owned();
    }
    (2..=taken.len() + 2)
        .map(|n| format!("{base}-{n}"))
        .find(|name| !taken.contains(name))
        .unwrap_or_else(|| base.to_owned())
}

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

/// The terminals `scope` covers, in input order.
#[must_use]
pub fn in_scope(panes: &[PaneFacts], scope: Scope) -> Vec<&PaneFacts> {
    panes.iter().filter(|pane| scope.covers(pane)).collect()
}

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

fn destination_of(candidates: &Candidates, option: &str) -> Option<Destination> {
    if option == STAY_OPTION {
        return Some(Destination::Stay);
    }
    candidates
        .by_option(option)
        .map(|candidate| candidate.destination.clone())
}

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

/// A name phux handed out rather than a person chose: a bare integer.
#[must_use]
pub fn generated_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skey(n: u8) -> SessionKey {
        SessionKey::new([n; 16]).expect("non-zero")
    }

    fn rkey(n: u8) -> ResourceKey {
        ResourceKey::new([n; 16]).expect("non-zero")
    }

    fn session(n: u8, name: &str, project: Option<&str>) -> SessionFacts {
        SessionFacts {
            key: skey(n),
            name: name.to_owned(),
            intake: false,
            project_id: project.map(|p| format!("github.com/o/{p}")),
            project_name: project.map(str::to_owned),
            checkout: project.map(|p| format!("/src/{p}")),
        }
    }

    fn pane(n: u8, session: u8, project: Option<&str>) -> PaneFacts {
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

    fn intake(n: u8) -> SessionFacts {
        SessionFacts {
            intake: true,
            ..session(n, "intake", None)
        }
    }

    fn run(sessions: &[SessionFacts], panes: &[PaneFacts], scope: Scope) -> Plan {
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

    /// A repository no session carries gets one new session, shared by
    /// every terminal working there, named after it and deduplicated.
    #[test]
    fn a_new_repository_gets_one_new_session() {
        let sessions = [intake(1), session(2, "web", None)];
        let panes = [
            pane(10, 1, Some("web")),
            pane(11, 2, Some("web")),
            pane(12, 1, Some("web")),
        ];
        let plan = run(&sessions, &panes, Scope::All);
        assert_eq!(plan.creates.len(), 1, "{plan:?}");
        assert_eq!(plan.creates[0].name, "web-2");
        assert_eq!(plan.moves.len(), 3);
        assert!(
            plan.moves
                .iter()
                .all(|m| m.to.is_none() && m.to_name == "web-2")
        );
    }

    /// Session scope renames no other session.
    #[test]
    fn session_scope_renames_only_itself() {
        let sessions = [session(1, "misc", None), session(2, "7", Some("web"))];
        let panes = [pane(10, 1, Some("web")), pane(11, 2, Some("web"))];
        let plan = run(&sessions, &panes, Scope::Session(skey(1)));
        assert!(plan.renames.is_empty(), "{plan:?}");
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(
            (plan.moves[0].to, plan.moves[0].to_name.as_str()),
            (Some(skey(2)), "7")
        );
        let plan = run(&sessions, &panes, Scope::Session(skey(2)));
        assert_eq!(plan.renames.len(), 1, "{plan:?}");
    }

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

    /// A generated-name session holding one repository takes its name and
    /// becomes its home: its terminal stays and an Intake terminal of the
    /// same repository joins it. A mixed one keeps its name; its repository
    /// terminal gets a new home.
    #[test]
    fn generated_names_adopt_their_repository() {
        let sessions = [intake(9), session(1, "3", None), session(2, "7", None)];
        let panes = [
            pane(10, 1, Some("phux")),
            pane(11, 2, Some("web")),
            pane(12, 2, None),
            pane(13, 9, Some("phux")),
        ];
        let plan = run(&sessions, &panes, Scope::All);
        assert_eq!(plan.creates.len(), 1, "{plan:?}");
        assert_eq!(plan.creates[0].name, "web");
        let into_phux: Vec<_> = plan
            .moves
            .iter()
            .filter(|m| m.to == Some(skey(1)))
            .collect();
        assert_eq!(into_phux.len(), 1, "{plan:?}");
        assert_eq!(
            (into_phux[0].pane, into_phux[0].to_name.as_str()),
            (rkey(13), "phux")
        );
        assert_eq!(
            plan.renames,
            vec![PlannedRename {
                session: skey(1),
                from: "3".into(),
                to: "phux".into()
            }]
        );
    }

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

    #[test]
    fn within_respects_path_boundaries() {
        assert!(within("/src/phux", "/src/phux/"));
        assert!(within("/src/phux/a", "/src/phux"));
        assert!(!within("/src/phuxx", "/src/phux"));
        assert!(!within("/a", ""));
    }
}
