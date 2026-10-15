//! Organize: put every terminal in the session it belongs in, in one step
//! (ADR-0170).
//!
//! [`gather`] reads the organization records, the snapshot, and the agent
//! records into planner facts; [`plan_or_fallback`] asks the judge
//! ([`phux_client_runtime::organize`]: `TypeSafe`'s Jev with a key, else
//! repository matching) and gates its verdicts in
//! [`phux_client_core::organize`]; [`apply`] creates sessions, moves
//! members, and renames sessions through the typed organization commands,
//! and returns a [`Journal`] that [`undo`] reverses. [`run`] is the whole
//! button; the CLI, the MCP tool, and the TUI all call it.
//!
//! Apply and undo have one implementation, the runtime's sans-IO
//! [`Sequencer`] (ADR-0171), which the desktop drives over the runtime's
//! asynchronous organization lane. Here it is driven over request/reply: a
//! [`Submitter`] that queues each submission, and a loop that sends each as
//! a request and feeds the reply back as its [`OrgOutcome`]. The sequencer
//! reads the records through the [`Records`] trait rather than a live
//! replica: a one-shot connection has no event stream to keep a replica
//! current, and a fresh `GET_ORGANIZATION` read ([`OrgInventory`]) answers
//! the same three questions (which server, where a terminal is, what a
//! session is called) without synthesizing replica pages from it. Moves go
//! through [`org::move_many`], which re-reads revisions and retries on
//! `CONFLICT`.
//!
//! Only metadata leaves the machine: names, titles, working directories,
//! agent providers, and repository identities. Terminal contents never do.

use std::cell::RefCell;
use std::collections::VecDeque;

use phux_client_core::org_ops::{OrganizationCommand, SubmitError};
use phux_client_core::organize::{self as planner, PaneFacts, Plan};
use phux_protocol::ids::{AuthorityId, OperationId, ResourceKey, SessionKey};
use phux_protocol::wire::frame::{Command, OrgFilter};
use serde_json::Value;

use crate::attach::connection::Connection;
use crate::glance::{self, Sources};
use crate::org::{self, OrgError, OrgInventory, OrgOutcome};

pub use phux_client_core::organize::{
    DEFAULT_MIN_CONFIDENCE, Facts, JOURNAL_SCHEMA, Journal, count, plan_lines,
};
use phux_client_runtime::organize::{
    PaneSeen, ProjectCache, Records, Sequencer, Submitter, UndoRefusal, session_facts,
};

pub use phux_client_runtime::organize::{
    API_KEY_ENV, INTERACTIVE_JEV_TIMEOUT, JEV_ENDPOINT, JEV_TIMEOUT, Judge, Options, PlanError,
    Report, Step, clear_journal, journal_path, load_journal, save_journal,
};

/// First request id for the agent-record reads.
const AGENT_READ_FIRST_REQUEST_ID: u32 = 0x8000_0000;

/// Why organizing failed before anything changed.
#[derive(Debug, thiserror::Error)]
pub enum OrganizeError {
    /// Reading or editing the records failed.
    #[error(transparent)]
    Org(#[from] OrgError),
    /// The snapshot could not be read.
    #[error("could not read state: {0}")]
    State(String),
    /// The session named as the scope does not exist.
    #[error("no such session: {0}")]
    NoSuchSession(String),
    /// Jev could not be reached or answered with an error.
    #[error("jev: {0}")]
    Jev(String),
    /// The undo record belongs to another server.
    #[error("the last organize run changed another server; nothing to undo here")]
    OtherServer,
}

impl From<PlanError> for OrganizeError {
    fn from(err: PlanError) -> Self {
        match err {
            PlanError::NoSuchSession(name) => Self::NoSuchSession(name),
            PlanError::Jev(why) => Self::Jev(why),
        }
    }
}

impl OrganizeError {
    /// A stable lowercase code for `--json` output.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Org(err) => err.code(),
            Self::State(_) => "transport",
            Self::NoSuchSession(_) => "no_such_target",
            Self::Jev(_) => "judge_failed",
            Self::OtherServer => "other_server",
        }
    }
}

/// Read everything the planner needs.
///
/// `files_host` names this machine when
/// the server shares its filesystem, so working directories resolve to
/// repositories; `None` (a remote server) uses only session attributes.
///
/// # Errors
///
/// A server without `ORGANIZATION`, or a failed read.
pub async fn gather(
    conn: &mut Connection,
    files_host: Option<&str>,
) -> Result<Facts, OrganizeError> {
    if !org::supported(conn) {
        return Err(OrgError::Unsupported.into());
    }
    let inventory = org::fetch(conn, OrgFilter::All, 1).await?;
    let view = crate::state::get_state_on(conn)
        .await
        .map_err(|err| OrganizeError::State(err.to_string()))?;
    let snapshot = view.snapshot();
    let terminals: Vec<_> = crate::resource::terminals(snapshot)
        .filter(|pane| pane.id.is_local())
        .map(|pane| pane.id.clone())
        .collect();
    let agents = crate::agent_record::fetch_index_on(
        conn,
        &terminals,
        AGENT_READ_FIRST_REQUEST_ID,
        &mut Vec::new(),
    )
    .await;
    let sources = Sources {
        snapshot,
        organization: Some(&inventory),
        agents: &agents,
    };
    let mut projects = ProjectCache::new(files_host);
    let names = org::KeyNames::of_inventory(&inventory);
    let mut panes = Vec::new();
    for session in &snapshot.sessions {
        let Some(record) = inventory
            .session_by_live(session.id)
            .or_else(|| inventory.session_named(&session.name))
        else {
            continue;
        };
        for member in glance::members(sources, session) {
            let Some(key) = member.key else { continue };
            panes.push(pane_facts(&names, record.key, &member, key, &mut projects));
        }
    }
    let sessions = inventory
        .sessions
        .iter()
        .map(|record| session_facts(record, inventory.intake()))
        .collect();
    Ok(Facts { sessions, panes })
}

fn pane_facts(
    names: &org::KeyNames,
    session: SessionKey,
    member: &glance::MemberGlance,
    key: ResourceKey,
    projects: &mut ProjectCache,
) -> PaneFacts {
    let label = member.live.as_ref().map_or_else(
        || names.display(*key.as_bytes()),
        crate::selector::format_terminal_id,
    );
    let seen = PaneSeen {
        label,
        title: member.title.clone(),
        cwd: member.cwd.clone(),
        agent: member.agent.clone(),
        status: member.status.clone(),
    };
    phux_client_runtime::organize::pane_facts(key, session, seen, projects)
}

/// The judge `config` and `offline` allow ([`Judge::configured`]), resolved
/// on a blocking thread because a key command may run.
pub async fn configured_judge(
    config: phux_config::OrganizeCfg,
    offline: bool,
) -> (Judge, Option<String>) {
    tokio::task::spawn_blocking(move || Judge::configured(&config, offline))
        .await
        .unwrap_or_else(|err| (Judge::Heuristic, Some(format!("judge setup failed: {err}"))))
}

/// [`phux_client_runtime::organize::plan_or_fallback`] on a blocking
/// thread: the plan, and why Jev was not used when it was asked for.
///
/// # Errors
///
/// [`OrganizeError::NoSuchSession`] for an unknown scope.
pub async fn plan_or_fallback(
    facts: &Facts,
    options: &Options,
) -> Result<(Plan, Option<String>), OrganizeError> {
    let (facts, options) = (facts.clone(), options.clone());
    tokio::task::spawn_blocking(move || {
        phux_client_runtime::organize::plan_or_fallback(&facts, &options)
    })
    .await
    .map_err(|err| OrganizeError::Jev(format!("planning failed: {err}")))?
    .map_err(OrganizeError::from)
}

/// Request id for organization requests: a one-shot connection has one
/// request in flight at a time.
const REQUEST_ID: u32 = 1;

impl Records for OrgInventory {
    fn authority(&self) -> Option<AuthorityId> {
        Some(self.authority)
    }

    fn session_of(&self, resource: &ResourceKey) -> Option<SessionKey> {
        self.resource(resource).map(|record| record.session)
    }

    fn session_name(&self, session: &SessionKey) -> Option<&str> {
        self.session(session).map(|record| record.name.as_str())
    }
}

/// A submission the [`Sequencer`] made, waiting to be sent.
enum Submission {
    /// One round trip: `SESSION_CREATE`, `SESSION_RENAME`, `SESSION_DELETE`.
    Command(Command),
    /// One `RESOURCE_MOVE_MANY`, rebuilt against fresh revisions when the
    /// server answers `CONFLICT` ([`org::move_many`]).
    Move {
        resources: Vec<ResourceKey>,
        to: SessionKey,
    },
}

/// The request/reply [`Submitter`]: it records each submission under a
/// fresh operation id for [`drive`] to send.
#[derive(Default)]
struct Queue(RefCell<VecDeque<(OperationId, Submission)>>);

impl Queue {
    fn push(&self, operation: OperationId, submission: Submission) -> OperationId {
        self.0.borrow_mut().push_back((operation, submission));
        operation
    }

    fn pop(&self) -> Option<(OperationId, Submission)> {
        self.0.borrow_mut().pop_front()
    }

    fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }
}

impl Submitter for Queue {
    fn submit(&self, command: Command) -> Result<OperationId, SubmitError> {
        let operation = *command
            .organization_operation()
            .ok_or(SubmitError::NotAnOperation)?;
        Ok(self.push(operation, Submission::Command(command)))
    }

    fn move_resources(
        &self,
        resources: &[ResourceKey],
        to: SessionKey,
    ) -> Result<Vec<OperationId>, SubmitError> {
        let resources = resources.to_vec();
        let submission = Submission::Move { resources, to };
        Ok(vec![self.push(org::mint_operation(), submission)])
    }

    fn rename_session(&self, session: SessionKey, name: &str) -> Result<OperationId, SubmitError> {
        self.submit(Command::SessionRename {
            operation: org::mint_operation(),
            session,
            name: name.to_owned(),
        })
    }

    fn delete_session(&self, session: SessionKey) -> Result<OperationId, SubmitError> {
        self.submit(Command::SessionDelete {
            operation: org::mint_operation(),
            session,
        })
    }
}

/// Run `sequencer` to its report over `conn`: send each submission as a
/// request, in the order made, and feed the reply back as its outcome.
///
/// The sequencer checks a phase's steps against the records only as it
/// enters that phase, which happens when the previous phase's last reply
/// lands; so `records` is read again just before that reply is fed. A
/// failed re-read keeps the older records: the moves are still
/// compare-and-swap against the server's current revisions.
async fn drive(conn: &mut Connection, mut sequencer: Sequencer, records: OrgInventory) -> Report {
    let queue = Queue::default();
    let mut records = records;
    let mut finished = sequencer.start(&records, &queue);
    loop {
        if let Some(report) = finished {
            return report;
        }
        let Some((operation, submission)) = queue.pop() else {
            return sequencer.report_so_far();
        };
        let outcome = send(conn, submission).await;
        if queue.is_empty() {
            records = org::fetch(conn, OrgFilter::All, REQUEST_ID)
                .await
                .unwrap_or(records);
        }
        finished = sequencer.on_outcome(operation, &outcome, &records, &queue);
    }
}

/// Send one submission and classify its reply.
async fn send(conn: &mut Connection, submission: Submission) -> OrgOutcome {
    let result = match submission {
        Submission::Command(command) => org::request(conn, REQUEST_ID, command)
            .await
            .map(|(outcome, _)| outcome),
        Submission::Move { resources, to } => org::move_many(conn, &resources, to)
            .await
            .map(moved_outcome),
    };
    result.unwrap_or_else(lost)
}

/// The outcome of a move the sequencer submitted. It submits at most one
/// server batch per move, so [`org::move_many`] answers one chunk.
fn moved_outcome(batches: Vec<org::BatchResult>) -> OrgOutcome {
    let mut outcome = OrgOutcome::NotSent {
        reason: "nothing to move".to_owned(),
    };
    for batch in batches {
        match batch.result {
            Ok(committed) => outcome = OrgOutcome::Committed(Box::new(committed)),
            Err(err) => return lost(err),
        }
    }
    outcome
}

/// A request that did not commit, as the outcome the sequencer reads: a
/// refusal or server error as such; a failure before sending as never
/// sent; anything after as unknown.
fn lost(err: OrgError) -> OrgOutcome {
    match err {
        OrgError::Refused(refusal) => OrgOutcome::Refused(refusal),
        OrgError::Server { code, message } => OrgOutcome::Error { code, message },
        unsent @ (OrgError::Unsupported | OrgError::NotFound(_) | OrgError::Ambiguous { .. }) => {
            OrgOutcome::NotSent {
                reason: unsent.to_string(),
            }
        }
        unknown @ (OrgError::OutcomeUnknown { .. }
        | OrgError::Unexpected(_)
        | OrgError::Attach(_)) => OrgOutcome::Unknown {
            reason: unknown.to_string(),
        },
    }
}

/// Carry out `plan`: create its sessions, move its terminals, then rename,
/// through the [`Sequencer`].
///
/// The records are read again first: a terminal no longer in the session
/// the plan saw it in (a person moved it while the judge thought) is
/// skipped. Moves go in one atomic batch per destination. A step that fails
/// is reported and the rest continue; a move into a session that failed to
/// create fails.
pub async fn apply(conn: &mut Connection, plan: &Plan) -> Report {
    match org::fetch(conn, OrgFilter::All, REQUEST_ID).await {
        Ok(records) => {
            let sequencer = Sequencer::apply(plan.clone(), Some(records.authority));
            drive(conn, sequencer, records).await
        }
        Err(err) => Report {
            journal: Journal::new(),
            failures: vec![Step {
                step: "read the records".to_owned(),
                code: err.code(),
                message: err.to_string(),
            }],
            skipped: Vec::new(),
        },
    }
}

/// Reverse `journal` against the current records, through the
/// [`Sequencer`].
///
/// Rename sessions back where they still carry the new name, move terminals
/// back that are still where they were put, then delete created sessions
/// left empty. Anything changed since is left alone and reported.
///
/// # Errors
///
/// A server without `ORGANIZATION`, a failed read, or
/// [`OrganizeError::OtherServer`] for another server's journal.
pub async fn undo(conn: &mut Connection, journal: &Journal) -> Result<Report, OrganizeError> {
    if !org::supported(conn) {
        return Err(OrgError::Unsupported.into());
    }
    let records = org::fetch(conn, OrgFilter::All, REQUEST_ID).await?;
    let sequencer = Sequencer::undo(journal.clone());
    if sequencer.refusal(&records) == Some(UndoRefusal::OtherServer) {
        return Err(OrganizeError::OtherServer);
    }
    Ok(drive(conn, sequencer, records).await)
}

/// Schema of [`document`].
pub const DOCUMENT_SCHEMA: u8 = 1;

/// The result document `phux organize --json` prints and `phux_organize`
/// returns.
#[must_use]
pub fn document(
    plan: &Plan,
    applied: Option<&Report>,
    judge_error: Option<&str>,
    dry_run: bool,
) -> Value {
    serde_json::json!({
        "schema_version": DOCUMENT_SCHEMA,
        "judge": plan.judge.as_str(),
        "judge_error": judge_error,
        "dry_run": dry_run,
        "panes_considered": plan.moves.len() + plan.kept.len(),
        "plan": plan,
        "applied": applied.map(|applied| &applied.journal),
        "failures": steps(applied.map_or(&[][..], |applied| applied.failures.as_slice())),
        "skipped": steps(applied.map_or(&[][..], |applied| applied.skipped.as_slice())),
    })
}

/// The result document `phux organize --undo --json` prints.
#[must_use]
pub fn undo_document(undone: &Report) -> Value {
    serde_json::json!({
        "schema_version": DOCUMENT_SCHEMA,
        "undone": undone.journal,
        "failures": steps(&undone.failures),
        "skipped": steps(&undone.skipped),
    })
}

/// Steps as the documents carry them: `{ step, code, message }`.
fn steps(steps: &[Step]) -> Value {
    steps
        .iter()
        .map(|step| {
            serde_json::json!({
                "step": step.step,
                "code": step.code,
                "message": step.message,
            })
        })
        .collect()
}

/// Gather, plan (Jev falling back to the heuristic), and, unless
/// `dry_run`, apply and keep the undo journal: the whole organize button.
///
/// # Errors
///
/// As [`gather`] and [`plan_or_fallback`]; per-step failures are in the
/// [`Report`].
pub async fn run(
    conn: &mut Connection,
    files_host: Option<&str>,
    options: &Options,
    dry_run: bool,
) -> Result<Outcome, OrganizeError> {
    let facts = gather(conn, files_host).await?;
    let (plan, judge_error) = plan_or_fallback(&facts, options).await?;
    let applied = if dry_run || plan.is_empty() {
        None
    } else {
        Some(apply(conn, &plan).await)
    };
    let journal_error = applied
        .as_ref()
        .filter(|applied| !applied.journal.is_empty())
        .and_then(|applied| save_journal(&applied.journal).err())
        .map(|err| err.to_string());
    Ok(Outcome {
        total_panes: facts.panes.len(),
        plan,
        applied,
        judge_error,
        journal_error,
    })
}

/// What one [`run`] did.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// Every terminal with a record.
    pub total_panes: usize,
    /// The plan.
    pub plan: Plan,
    /// What applying it did; `None` for a dry run or an empty plan.
    pub applied: Option<Report>,
    /// Why Jev was not used when it was asked for.
    pub judge_error: Option<String>,
    /// Why the undo journal could not be kept.
    pub journal_error: Option<String>,
}

impl Outcome {
    /// Whether any applied step failed.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.applied
            .as_ref()
            .is_some_and(|applied| !applied.failures.is_empty())
    }

    /// One short line for a status bar or toast.
    #[must_use]
    pub fn summary(&self) -> String {
        match &self.applied {
            Some(applied) => {
                planner::journal_summary(self.plan.judge, &applied.journal, applied.failures.len())
            }
            None if self.plan.is_empty() => "organize: every pane is where it belongs".to_owned(),
            None => format!("organize: {} moves planned", self.plan.moves.len()),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::testkit::{OrgFixture, ScriptSpec};
    use phux_client_core::organize::{JudgeKind, PlannedRename};
    use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
    use phux_protocol::wire::frame::{ExecState, SessionLifetime};

    fn org_spec(fixture: OrgFixture) -> ScriptSpec {
        ScriptSpec::new()
            .server_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
            .organization(fixture)
    }

    /// A terminal moved by someone else after planning is skipped, not
    /// moved back over their edit, and not journaled.
    #[tokio::test]
    async fn apply_skips_terminals_moved_since_the_plan() {
        let mut fixture = OrgFixture::new();
        let planned_from = fixture.session("misc", SessionLifetime::Persistent);
        let now_in = fixture.session("elsewhere", SessionLifetime::Persistent);
        let target = fixture.session("phux", SessionLifetime::Persistent);
        let pane = fixture.resource(now_in, Some(ExecState::Running));
        let dir = tempfile::tempdir().expect("temp dir");
        let (socket, server) = crate::testkit::serve_one(dir.path(), org_spec(fixture));
        let mut conn = Connection::connect(&socket).await.expect("connect");
        let plan = Plan {
            judge: JudgeKind::Jev,
            creates: Vec::new(),
            moves: vec![planner::PlannedMove {
                pane,
                label: "@1".into(),
                from: planned_from,
                from_name: "misc".into(),
                to: Some(target),
                to_name: "phux".into(),
                confidence: 0.9,
            }],
            renames: Vec::new(),
            kept: Vec::new(),
        };
        let applied = apply(&mut conn, &plan).await;
        assert!(applied.failures.is_empty(), "{:?}", applied.failures);
        assert_eq!(applied.skipped.len(), 1);
        assert!(applied.journal.moved.is_empty());
        let after = org::fetch(&mut conn, OrgFilter::All, 1)
            .await
            .expect("fetch");
        assert_eq!(after.resource(&pane).map(|r| r.session), Some(now_in));
        drop(conn);
        server.await.expect("scripted server");
    }

    /// A plan's creates, moves, and renames ride the typed commands, and
    /// undo reverses them: members return, renames revert, and the created
    /// session is deleted.
    #[tokio::test]
    async fn apply_then_undo_round_trips() {
        let mut fixture = OrgFixture::new();
        let home = fixture.session("misc", SessionLifetime::Persistent);
        let pane = fixture.resource(home, Some(ExecState::Running));
        let dir = tempfile::tempdir().expect("temp dir");
        let (socket, server) = crate::testkit::serve_one(dir.path(), org_spec(fixture));
        let mut conn = Connection::connect(&socket).await.expect("connect");
        let plan = Plan {
            judge: JudgeKind::Heuristic,
            creates: vec![planner::NewSession {
                name: "phux".into(),
                project_id: "github.com/no-phux/phux".into(),
                project_name: "phux".into(),
                checkout: "/src/phux".into(),
            }],
            moves: vec![planner::PlannedMove {
                pane,
                label: "@1".into(),
                from: home,
                from_name: "misc".into(),
                to: None,
                to_name: "phux".into(),
                confidence: 1.0,
            }],
            renames: vec![PlannedRename {
                session: home,
                from: "misc".into(),
                to: "tidy".into(),
            }],
            kept: Vec::new(),
        };
        let applied = apply(&mut conn, &plan).await;
        assert!(applied.failures.is_empty(), "{:?}", applied.failures);
        assert_eq!(applied.journal.created.len(), 1);
        assert_eq!(applied.journal.moved.len(), 1);
        assert_eq!(applied.journal.renamed.len(), 1);
        let after = org::fetch(&mut conn, OrgFilter::All, 1)
            .await
            .expect("fetch");
        let created = applied.journal.created[0].key;
        assert_eq!(after.resource(&pane).map(|r| r.session), Some(created));
        assert_eq!(after.session(&home).map(|s| s.name.as_str()), Some("tidy"));

        let undone = undo(&mut conn, &applied.journal).await.expect("undo");
        assert!(undone.failures.is_empty(), "{:?}", undone.failures);
        let after = org::fetch(&mut conn, OrgFilter::All, 1)
            .await
            .expect("fetch");
        assert_eq!(after.resource(&pane).map(|r| r.session), Some(home));
        assert_eq!(after.session(&home).map(|s| s.name.as_str()), Some("misc"));
        assert!(after.session(&created).is_none());
        drop(conn);
        server.await.expect("scripted server");
    }

    /// Another server's journal is refused before anything is sent, and
    /// a step keeps its code in the `--json` documents.
    #[tokio::test]
    async fn undo_refuses_another_servers_journal() {
        let mut fixture = OrgFixture::new();
        fixture.session("misc", SessionLifetime::Persistent);
        let dir = tempfile::tempdir().expect("temp dir");
        let (socket, server) = crate::testkit::serve_one(dir.path(), org_spec(fixture));
        let mut conn = Connection::connect(&socket).await.expect("connect");
        let mut journal = Journal::new();
        journal.authority = Some(AuthorityId::new([0xee; 16]).expect("non-zero"));
        let refused = undo(&mut conn, &journal).await;
        assert!(
            matches!(refused, Err(OrganizeError::OtherServer)),
            "{refused:?}"
        );
        drop(conn);
        server.await.expect("scripted server");

        let report = Report {
            journal: Journal::new(),
            failures: vec![Step {
                step: "delete web".into(),
                code: "not_empty",
                message: "it holds work".into(),
            }],
            skipped: Vec::new(),
        };
        let document = undo_document(&report);
        assert_eq!(document["failures"][0]["code"], "not_empty");
        assert_eq!(document["failures"][0]["step"], "delete web");
    }
}
