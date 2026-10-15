//! The destinations a judge chooses among: every session but Intake, plus
//! one proposed session per repository no session carries, after
//! generated-name sessions are adopted by their repository.

use std::collections::HashSet;

use phux_protocol::ids::{ResourceKey, SessionKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::facts::{PaneFacts, Scope, SessionFacts, insert_some};
use super::plan::PlannedRename;

/// Most options one Choice question may carry (the API allows 255).
pub const MAX_OPTIONS: usize = 255;

/// Member summaries shown per session in a Choice option.
const MEMBERS_PER_OPTION: usize = 6;

/// The option that leaves a terminal where it is when no session fits.
pub(super) const STAY_OPTION: &str = "leave where it is";

/// A session the plan creates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSession {
    /// Its name, unique among the sessions the planner saw.
    pub name: String,
    /// The project identity it carries (`project.id`).
    pub project_id: String,
    /// The project's display name (`project.name`), which the session name
    /// may differ from by a numeric suffix.
    pub project_name: String,
    /// The project's checkout root (`project.checkout`).
    pub checkout: String,
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
    pub(super) fn criterion(&self, asked: ResourceKey) -> Value {
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
    distinct_options(&mut options);
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

/// Make every option text distinct, and distinct from [`STAY_OPTION`]: a
/// later duplicate gains ` #2`, ` #3`, ... so an answer names exactly one
/// destination and the judge sees every option's own description.
fn distinct_options(options: &mut [Candidate]) {
    let mut seen: HashSet<String> = HashSet::from([STAY_OPTION.to_owned()]);
    for candidate in options.iter_mut() {
        if seen.contains(&candidate.option) {
            let base = candidate.option.clone();
            candidate.option = (2..=seen.len() + 1)
                .map(|n| format!("{base} #{n}"))
                .find(|option| !seen.contains(option))
                .unwrap_or(base);
        }
        seen.insert(candidate.option.clone());
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
        if let Some(home) = named_for(&out[index], panes) {
            // An earlier run adopted it: the name says which repository it
            // holds, though no attribute does.
            let session = &mut out[index];
            session.project_id = Some(home.id);
            session.project_name = Some(home.name);
            session.checkout = home.checkout;
            continue;
        }
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

/// The repository a session without one is named after: a member works in
/// a repository whose name is the session's name. A session an earlier run
/// renamed carries no repository attribute (no command sets one on an
/// existing session), so its name is the only record of the adoption.
fn named_for(session: &SessionFacts, panes: &[PaneFacts]) -> Option<Home> {
    if session.intake || session.project_id.is_some() {
        return None;
    }
    let pane = panes.iter().find(|pane| {
        pane.session == session.key && pane.project_name.as_deref() == Some(session.name.as_str())
    })?;
    Some(Home {
        id: pane.project_id.clone()?,
        name: session.name.clone(),
        checkout: pane.checkout.clone(),
    })
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
    let checkout = pane.checkout.clone()?;
    let project_name = pane
        .project_name
        .clone()
        .unwrap_or_else(|| repo_name_of(id));
    let name = unique_name(&project_name, taken);
    taken.insert(name.clone());
    Some(NewSession {
        name,
        project_id: id.to_owned(),
        project_name,
        checkout,
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

/// A name phux handed out rather than a person chose: a bare integer.
#[must_use]
pub fn generated_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::organize::jev::destination_of;
    use crate::organize::plan::KeptReason;
    use crate::organize::testkit::*;

    /// A repository no session carries gets one new session, shared by
    /// every terminal working there, named after it and deduplicated.
    #[test]
    fn a_new_repository_gets_one_new_session() {
        let sessions = [
            intake(1),
            session(2, "misc", None),
            session(3, "web", Some("other")),
        ];
        let panes = [
            pane(10, 1, Some("web")),
            pane(11, 2, Some("web")),
            pane(12, 1, Some("web")),
        ];
        let plan = run(&sessions, &panes, Scope::All);
        assert_eq!(plan.creates.len(), 1, "{plan:?}");
        assert_eq!(plan.creates[0].name, "web-2");
        assert_eq!(plan.creates[0].project_name, "web");
        assert_eq!(plan.moves.len(), 3);
        assert!(
            plan.moves
                .iter()
                .all(|m| m.to.is_none() && m.to_name == "web-2")
        );
    }

    /// A session named after a repository, carrying no attribute, is that
    /// repository's home: no twin is created.
    #[test]
    fn a_session_named_for_a_repository_is_its_home() {
        let sessions = [intake(1), session(2, "web", None)];
        let panes = [pane(10, 1, Some("web")), pane(11, 2, Some("web"))];
        let plan = run(&sessions, &panes, Scope::All);
        assert!(plan.creates.is_empty(), "{plan:?}");
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(plan.moves[0].to, Some(skey(2)));
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

    /// Options are distinct: a session named like a proposal or like the
    /// stay option keeps its own key, and answers resolve to the right one.
    #[test]
    fn option_texts_never_collide() {
        let sessions = [
            session(1, "misc", None),
            session(2, "web (new session)", None),
            session(3, STAY_OPTION, None),
        ];
        let panes = [pane(10, 1, Some("web"))];
        let candidates = candidates(&sessions, &panes, Scope::All);
        let options: HashSet<&str> = candidates
            .options
            .iter()
            .map(|c| c.option.as_str())
            .collect();
        assert_eq!(options.len(), candidates.options.len());
        assert!(!options.contains(STAY_OPTION));
        let new = candidates
            .options
            .iter()
            .find(|c| matches!(c.destination, Destination::New(_)))
            .expect("proposal");
        assert_ne!(new.option, "web (new session)");
        assert_eq!(
            destination_of(&candidates, "web (new session)"),
            Some(Destination::Existing(skey(2)))
        );
        assert_eq!(
            destination_of(&candidates, &new.option),
            Some(Destination::New(0))
        );
    }

    /// A second run after an adoption is a no-op: the renamed session is
    /// recognized by name as its repository's home.
    #[test]
    fn a_second_run_after_adoption_moves_nothing() {
        let sessions = [session(1, "phux", None)];
        let panes = [pane(10, 1, Some("phux")), pane(11, 1, Some("phux"))];
        let plan = run(&sessions, &panes, Scope::All);
        assert!(plan.is_empty(), "{plan:?}");
        assert!(plan.kept.iter().all(|k| k.reason == KeptReason::InPlace));
    }
}
