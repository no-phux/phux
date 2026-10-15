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
//! Only metadata leaves the machine: names, titles, working directories,
//! agent providers, and repository identities. Terminal contents never do.

use std::collections::{BTreeMap, HashMap};

use phux_client_core::organize::{
    self as planner, CreatedSession, MovedPane, PaneFacts, Plan, PlannedRename,
};
use phux_protocol::ids::{ResourceKey, SessionKey};
use phux_protocol::wire::frame::{Command, OrgFilter, Refusal, SessionLifetime};
use serde::Serialize;
use serde_json::Value;

use crate::attach::connection::Connection;
use crate::glance::{self, Sources};
use crate::org::{self, OrgError, OrgInventory};

pub use phux_client_core::organize::{
    DEFAULT_MIN_CONFIDENCE, Facts, JOURNAL_SCHEMA, Journal, count, plan_lines,
};
use phux_client_runtime::organize::{PaneSeen, ProjectCache, session_facts};

pub use phux_client_runtime::organize::{
    API_KEY_ENV, INTERACTIVE_JEV_TIMEOUT, JEV_ENDPOINT, JEV_TIMEOUT, Judge, Options, PlanError,
    clear_journal, journal_path, load_journal, save_journal,
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

/// One step that did not take effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepFailure {
    /// What the step was (`create web`, `move @3 to web`).
    pub step: String,
    /// The refusal's code.
    pub code: &'static str,
    /// Why.
    pub message: String,
}

/// What [`apply`] or [`undo`] did.
#[derive(Debug, Clone, Default)]
pub struct Applied {
    /// What took effect.
    pub journal: Journal,
    /// Steps the server refused or that could not be sent.
    pub failures: Vec<StepFailure>,
    /// Steps left alone because the records changed since the plan (or the
    /// run) was made; not failures.
    pub skipped: Vec<StepFailure>,
}

/// Carry out `plan`: create its sessions, move its terminals, then rename.
///
/// The records are read again first: a terminal no longer in the session
/// the plan saw it in (a person moved it while the judge thought) is
/// skipped. Moves go in one atomic batch per destination. A step that fails
/// is reported and the rest continue; a move into a session that failed to
/// create is skipped.
pub async fn apply(conn: &mut Connection, plan: &Plan) -> Applied {
    let mut applied = Applied {
        journal: Journal::new(),
        ..Applied::default()
    };
    let inventory = match org::fetch(conn, OrgFilter::All, 1).await {
        Ok(inventory) => inventory,
        Err(err) => {
            applied.fail("read the records".to_owned(), &err);
            return applied;
        }
    };
    applied.journal.authority = Some(inventory.authority);
    let mut new_keys: HashMap<String, SessionKey> = HashMap::new();
    for new in &plan.creates {
        match create_session(conn, new).await {
            Ok(key) => {
                new_keys.insert(new.name.clone(), key);
                applied.journal.created.push(CreatedSession {
                    key,
                    name: new.name.clone(),
                });
            }
            Err(err) => applied.fail(format!("create {}", new.name), &err),
        }
    }
    for (to, steps) in batches(plan, &inventory, &new_keys, &mut applied) {
        move_batch(conn, to, &steps, &mut applied).await;
    }
    for rename in &plan.renames {
        match rename_session(conn, rename.session, &rename.to).await {
            Ok(()) => applied.journal.renamed.push(rename.clone()),
            Err(err) => applied.fail(format!("rename {} to {}", rename.from, rename.to), &err),
        }
    }
    applied
}

/// The plan's moves by destination, each still valid: a terminal no longer
/// where the plan saw it is skipped, and a move into a session that failed
/// to create fails.
fn batches<'p>(
    plan: &'p Plan,
    inventory: &OrgInventory,
    new_keys: &HashMap<String, SessionKey>,
    applied: &mut Applied,
) -> BTreeMap<SessionKey, Vec<&'p planner::PlannedMove>> {
    let mut batches: BTreeMap<SessionKey, Vec<&planner::PlannedMove>> = BTreeMap::new();
    for step in &plan.moves {
        let label = format!("move {} to {}", step.label, step.to_name);
        let still_there = inventory
            .resource(&step.pane)
            .is_some_and(|record| record.session == step.from);
        if !still_there {
            applied.skip(label, "moved or gone since the plan");
            continue;
        }
        let Some(to) = step.to.or_else(|| new_keys.get(&step.to_name).copied()) else {
            applied.failures.push(StepFailure {
                step: label,
                code: "not_found",
                message: format!("session {:?} was not created", step.to_name),
            });
            continue;
        };
        batches.entry(to).or_default().push(step);
    }
    batches
}

impl Applied {
    fn fail(&mut self, step: String, err: &OrgError) {
        self.failures.push(StepFailure {
            step,
            code: err.code(),
            message: err.to_string(),
        });
    }
}

async fn move_batch(
    conn: &mut Connection,
    to: SessionKey,
    steps: &[&planner::PlannedMove],
    applied: &mut Applied,
) {
    let keys: Vec<ResourceKey> = steps.iter().map(|step| step.pane).collect();
    let results = match org::move_many(conn, &keys, to).await {
        Ok(results) => results,
        Err(err) => {
            let name = steps.first().map_or("", |step| step.to_name.as_str());
            applied.fail(format!("move {} terminals to {name}", keys.len()), &err);
            return;
        }
    };
    let from: HashMap<ResourceKey, SessionKey> =
        steps.iter().map(|step| (step.pane, step.from)).collect();
    for batch in results {
        match batch.result {
            Ok(_) => applied
                .journal
                .moved
                .extend(batch.resources.iter().filter_map(|pane| {
                    Some(MovedPane {
                        pane: *pane,
                        from: *from.get(pane)?,
                        to,
                    })
                })),
            Err(err) => applied.fail(format!("move {} terminals", batch.resources.len()), &err),
        }
    }
}

async fn create_session(
    conn: &mut Connection,
    new: &planner::NewSession,
) -> Result<SessionKey, OrgError> {
    let attrs = phux_client_runtime::organize::creation_attrs(new);
    let command = Command::SessionCreate {
        operation: org::mint_operation(),
        name: new.name.clone(),
        lifetime: SessionLifetime::Persistent,
        attrs,
        launch: None,
    };
    let committed = org::commit(conn, 1, command).await?;
    committed
        .session
        .ok_or_else(|| OrgError::Unexpected("create answered without a session".to_owned()))
}

async fn rename_session(
    conn: &mut Connection,
    session: SessionKey,
    name: &str,
) -> Result<(), OrgError> {
    let command = Command::SessionRename {
        operation: org::mint_operation(),
        session,
        name: name.to_owned(),
    };
    org::commit(conn, 1, command).await.map(|_| ())
}

/// Reverse `journal` against the current records.
///
/// Rename sessions back
/// where they still carry the new name, move terminals back that are still
/// where they were put, then delete created sessions left empty. Anything
/// changed since is left alone and reported.
///
/// # Errors
///
/// A server without `ORGANIZATION`, or a failed read.
pub async fn undo(conn: &mut Connection, journal: &Journal) -> Result<Applied, OrganizeError> {
    if !org::supported(conn) {
        return Err(OrgError::Unsupported.into());
    }
    let inventory = org::fetch(conn, OrgFilter::All, 1).await?;
    if journal
        .authority
        .is_some_and(|authority| authority != inventory.authority)
    {
        return Err(OrganizeError::OtherServer);
    }
    let mut applied = Applied::default();
    undo_renames(conn, &inventory, &journal.renamed, &mut applied).await;
    let back = moves_back(&inventory, &journal.moved, &mut applied);
    for (to, panes) in back {
        undo_batch(conn, &inventory, to, &panes, &mut applied).await;
    }
    for created in &journal.created {
        let step = format!("delete {}", created.name);
        match org::delete_session(conn, created.key).await {
            Ok(_) => applied.journal.created.push(created.clone()),
            Err(OrgError::Refused(Refusal::NotEmpty { .. })) => {
                applied.skip(step, "it holds work now; left in place");
            }
            Err(OrgError::Refused(Refusal::NotFound)) => applied.skip(step, "already gone"),
            Err(err) => applied.fail(step, &err),
        }
    }
    Ok(applied)
}

/// Rename each session back that still carries the name the run gave it.
async fn undo_renames(
    conn: &mut Connection,
    inventory: &OrgInventory,
    renamed: &[PlannedRename],
    applied: &mut Applied,
) {
    for rename in renamed.iter().rev() {
        let step = format!("rename {} back to {}", rename.to, rename.from);
        let current = inventory.session(&rename.session).map(|s| s.name.as_str());
        if current != Some(rename.to.as_str()) {
            applied.skip(step, "changed since");
            continue;
        }
        match rename_session(conn, rename.session, &rename.from).await {
            Ok(()) => applied.journal.renamed.push(PlannedRename {
                session: rename.session,
                from: rename.to.clone(),
                to: rename.from.clone(),
            }),
            Err(err) => applied.fail(step, &err),
        }
    }
}

/// The moves to reverse, by the session each pane returns to: only panes
/// still where the run put them, whose old session still exists.
fn moves_back(
    inventory: &OrgInventory,
    moved: &[MovedPane],
    applied: &mut Applied,
) -> BTreeMap<SessionKey, Vec<ResourceKey>> {
    let mut back: BTreeMap<SessionKey, Vec<ResourceKey>> = BTreeMap::new();
    for moved in moved {
        let still_there = inventory
            .resource(&moved.pane)
            .is_some_and(|record| record.session == moved.to);
        if still_there && inventory.session(&moved.from).is_some() {
            back.entry(moved.from).or_default().push(moved.pane);
        } else {
            applied.skip(format!("move {} back", moved.pane), "moved or gone since");
        }
    }
    back
}

async fn undo_batch(
    conn: &mut Connection,
    inventory: &OrgInventory,
    to: SessionKey,
    panes: &[ResourceKey],
    applied: &mut Applied,
) {
    let results = match org::move_many(conn, panes, to).await {
        Ok(results) => results,
        Err(err) => {
            applied.fail(format!("move {} terminals back", panes.len()), &err);
            return;
        }
    };
    for batch in results {
        match batch.result {
            Ok(_) => applied
                .journal
                .moved
                .extend(batch.resources.iter().map(|pane| MovedPane {
                    pane: *pane,
                    from: inventory.resource(pane).map_or(to, |record| record.session),
                    to,
                })),
            Err(err) => applied.fail(
                format!("move {} terminals back", batch.resources.len()),
                &err,
            ),
        }
    }
}

impl Applied {
    fn skip(&mut self, step: String, why: &str) {
        self.skipped.push(StepFailure {
            step,
            code: "skipped",
            message: why.to_owned(),
        });
    }
}

/// Schema of [`document`].
pub const DOCUMENT_SCHEMA: u8 = 1;

/// The result document `phux organize --json` prints and `phux_organize`
/// returns.
#[must_use]
pub fn document(
    plan: &Plan,
    applied: Option<&Applied>,
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
        "failures": applied.map_or(&[][..], |applied| applied.failures.as_slice()),
        "skipped": applied.map_or(&[][..], |applied| applied.skipped.as_slice()),
    })
}

/// Gather, plan (Jev falling back to the heuristic), and, unless
/// `dry_run`, apply and keep the undo journal: the whole organize button.
///
/// # Errors
///
/// As [`gather`] and [`plan_or_fallback`]; per-step failures are in the
/// [`Applied`].
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
    pub applied: Option<Applied>,
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
    use phux_client_core::organize::JudgeKind;
    use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
    use phux_protocol::wire::frame::ExecState;

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
}
