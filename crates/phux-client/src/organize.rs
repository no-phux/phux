//! Organize: put every terminal in the session it belongs in, in one step
//! (ADR-0170).
//!
//! [`gather`] reads the organization records, the snapshot, and the agent
//! records into planner facts; [`plan_with`] asks a judge (`TypeSafe`'s Jev
//! when `TYPESAFE_API_KEY` is set, else repository matching) and gates its
//! verdicts in [`phux_client_core::organize`]; [`apply`] creates sessions,
//! moves members, and renames sessions through the typed organization
//! commands, and returns a [`Journal`] that [`undo`] reverses. The CLI, the
//! MCP tool, and the TUI all run this one path.
//!
//! Only metadata leaves the machine: names, titles, working directories,
//! agent providers, and repository identities. Terminal contents never do.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use phux_client_core::organize::{
    self as planner, Candidates, JudgeKind, PaneFacts, Plan, PlanInput, PlannedRename, Scope,
    SessionFacts, Verdict,
};
use phux_core::project_identity::SessionProject;
use phux_protocol::ids::{ResourceKey, SessionKey};
use phux_protocol::wire::frame::{Command, OrgFilter, SessionLifetime};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncWriteExt;

use crate::attach::connection::Connection;
use crate::glance::{self, Sources};
use crate::org::{self, OrgError, OrgInventory};

pub use phux_client_core::organize::DEFAULT_MIN_CONFIDENCE;

/// The Jev evaluation endpoint.
pub const JEV_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";

/// The environment variable holding the `TypeSafe` API key.
pub const API_KEY_ENV: &str = "TYPESAFE_API_KEY";

/// How long one Jev request may take by default.
pub const JEV_TIMEOUT: Duration = Duration::from_secs(60);

/// How long `organize.jev-key-command` may take.
const KEY_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

/// First request id for the agent-record reads.
const AGENT_READ_FIRST_REQUEST_ID: u32 = 0x8000_0000;

/// Schema of the [`Journal`] document.
const JOURNAL_SCHEMA: u8 = 1;

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
        }
    }
}

/// What one organize run read.
#[derive(Debug, Clone)]
pub struct Facts {
    /// Every session.
    pub sessions: Vec<SessionFacts>,
    /// Every terminal with a record.
    pub panes: Vec<PaneFacts>,
}

impl Facts {
    /// The session named `name`.
    #[must_use]
    pub fn session_named(&self, name: &str) -> Option<&SessionFacts> {
        self.sessions.iter().find(|session| session.name == name)
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
        .map(|record| session_facts(&inventory, record))
        .collect();
    Ok(Facts { sessions, panes })
}

fn session_facts(
    inventory: &OrgInventory,
    record: &phux_protocol::wire::frame::SessionRecord,
) -> SessionFacts {
    let project = SessionProject::from_attrs(
        record
            .attrs
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    SessionFacts {
        key: record.key,
        name: record.name.clone(),
        intake: inventory.intake() == Some(record.key),
        project_id: project.as_ref().map(|p| p.id.clone()),
        project_name: project.as_ref().map(|p| p.name.clone()),
        checkout: project.map(|p| p.checkout),
    }
}

fn pane_facts(
    names: &org::KeyNames,
    session: SessionKey,
    member: &glance::MemberGlance,
    key: ResourceKey,
    projects: &mut ProjectCache<'_>,
) -> PaneFacts {
    let label = member.live.as_ref().map_or_else(
        || names.display(*key.as_bytes()),
        crate::selector::format_terminal_id,
    );
    let project = member.cwd.as_deref().and_then(|cwd| projects.of(cwd));
    PaneFacts {
        key,
        label,
        session,
        title: member.title.clone(),
        cwd: member.cwd.clone(),
        agent: member.agent.clone(),
        status: member.status.clone(),
        project_id: project.as_ref().map(|p| p.id.clone()),
        project_name: project.as_ref().map(|p| p.name.clone()),
        checkout: project.map(|p| p.checkout),
    }
}

/// Repository discovery per working directory, read once each.
struct ProjectCache<'a> {
    host: Option<&'a str>,
    seen: HashMap<String, Option<SessionProject>>,
}

impl<'a> ProjectCache<'a> {
    fn new(host: Option<&'a str>) -> Self {
        Self {
            host,
            seen: HashMap::new(),
        }
    }

    fn of(&mut self, cwd: &str) -> Option<SessionProject> {
        let host = self.host?;
        self.seen
            .entry(cwd.to_owned())
            .or_insert_with(|| crate::project_identity::discover(Path::new(cwd), host, None))
            .clone()
    }
}

/// Which judge to ask.
#[derive(Debug, Clone)]
pub enum Judge {
    /// `TypeSafe`'s Jev, with this API key.
    Jev(String),
    /// Repository and directory matching only.
    Heuristic,
}

impl Judge {
    /// Jev when [`API_KEY_ENV`] is set and `offline` is false, else the
    /// heuristic.
    #[must_use]
    pub fn from_env(offline: bool) -> Self {
        if offline {
            return Self::Heuristic;
        }
        std::env::var(API_KEY_ENV)
            .ok()
            .filter(|key| !key.trim().is_empty())
            .map_or(Self::Heuristic, Self::Jev)
    }

    /// The judge `config` and `offline` allow: the heuristic when either is
    /// offline; Jev with [`API_KEY_ENV`], else with the key
    /// `organize.jev-key-command` prints; else the heuristic. The second
    /// value says why Jev was wanted but is not used.
    pub async fn configured(
        config: &phux_config::OrganizeCfg,
        offline: bool,
    ) -> (Self, Option<String>) {
        let judge = Self::from_env(offline || config.offline);
        let argv = config.jev_key_command.as_deref().unwrap_or_default();
        if matches!(judge, Self::Jev(_)) || offline || config.offline || argv.is_empty() {
            return (judge, None);
        }
        match key_from_command(argv).await {
            Ok(key) => (Self::Jev(key), None),
            Err(why) => (
                Self::Heuristic,
                Some(format!("organize.jev-key-command: {why}")),
            ),
        }
    }

    /// Which kind of judge this is.
    #[must_use]
    pub const fn kind(&self) -> JudgeKind {
        match self {
            Self::Jev(_) => JudgeKind::Jev,
            Self::Heuristic => JudgeKind::Heuristic,
        }
    }
}

/// How to plan.
#[derive(Debug, Clone)]
pub struct Options {
    /// Which terminals may move: every one, or one session's.
    pub session: Option<String>,
    /// Who decides.
    pub judge: Judge,
    /// Confidence a verdict needs to move a terminal.
    pub min_confidence: f64,
    /// How long one Jev request may take before the heuristic decides.
    pub jev_timeout: Duration,
}

/// Plan an organize run over `facts`.
///
/// # Errors
///
/// [`OrganizeError::NoSuchSession`] for an unknown scope;
/// [`OrganizeError::Jev`] when Jev fails (nothing has changed).
pub async fn plan_with(facts: &Facts, options: &Options) -> Result<Plan, OrganizeError> {
    let scope = match options.session.as_deref() {
        None => Scope::All,
        Some(name) => Scope::Session(
            facts
                .session_named(name)
                .ok_or_else(|| OrganizeError::NoSuchSession(name.to_owned()))?
                .key,
        ),
    };
    let candidates = planner::candidates(&facts.sessions, &facts.panes, scope);
    let verdicts = match &options.judge {
        Judge::Jev(key) => {
            jev_verdicts(key, facts, scope, &candidates, options.jev_timeout).await?
        }
        Judge::Heuristic => planner::heuristic_verdicts(&facts.panes, scope, &candidates),
    };
    Ok(planner::plan(
        PlanInput {
            panes: &facts.panes,
            scope,
            candidates: &candidates,
            judge: options.judge.kind(),
            min_confidence: options.min_confidence,
        },
        &verdicts,
    ))
}

/// [`plan_with`], falling back to the heuristic when Jev fails: the plan,
/// and why Jev was not used when it was asked for.
///
/// # Errors
///
/// [`OrganizeError::NoSuchSession`] for an unknown scope.
pub async fn plan_or_fallback(
    facts: &Facts,
    options: &Options,
) -> Result<(Plan, Option<String>), OrganizeError> {
    match plan_with(facts, options).await {
        Err(OrganizeError::Jev(why)) => {
            let fallback = Options {
                judge: Judge::Heuristic,
                ..options.clone()
            };
            Ok((plan_with(facts, &fallback).await?, Some(why)))
        }
        other => other.map(|plan| (plan, None)),
    }
}

/// The trimmed stdout of `argv`, which must exit 0 and print something.
async fn key_from_command(argv: &[String]) -> Result<String, String> {
    let (program, rest) = argv.split_first().ok_or("empty command")?;
    let child = tokio::process::Command::new(program)
        .args(rest)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(KEY_COMMAND_TIMEOUT, child)
        .await
        .map_err(|_| "timed out".to_owned())?
        .map_err(|err| format!("cannot run {program}: {err}"))?;
    if !output.status.success() {
        return Err(format!("{program} exited with {}", output.status));
    }
    let key = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if key.is_empty() {
        return Err(format!("{program} printed nothing"));
    }
    Ok(key)
}

async fn jev_verdicts(
    key: &str,
    facts: &Facts,
    scope: Scope,
    candidates: &Candidates,
    timeout: Duration,
) -> Result<Vec<Verdict>, OrganizeError> {
    let mut responses = Vec::new();
    for body in planner::jev_request(&facts.panes, scope, candidates) {
        responses.push(jev_post(key, &body, timeout).await?);
    }
    planner::read_jev_answers(&facts.panes, scope, candidates, &responses)
        .map_err(|err| OrganizeError::Jev(err.to_string()))
}

/// POST one request body to Jev through `curl`, the API key and body on its
/// stdin so neither appears in a process listing.
async fn jev_post(key: &str, body: &Value, timeout: Duration) -> Result<Value, OrganizeError> {
    let config = curl_config(key, body, timeout);
    let mut child = tokio::process::Command::new("curl")
        .args(["--silent", "--show-error", "--config", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| OrganizeError::Jev(format!("cannot run curl: {err}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(config.as_bytes())
            .await
            .map_err(|err| OrganizeError::Jev(format!("cannot write to curl: {err}")))?;
    }
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| OrganizeError::Jev("timed out".to_owned()))?
        .map_err(|err| OrganizeError::Jev(err.to_string()))?;
    read_jev_reply(&output.stdout, &output.stderr, output.status.success())
}

/// A curl config that POSTs `body` to [`JEV_ENDPOINT`] with `key`.
fn curl_config(key: &str, body: &Value, timeout: Duration) -> String {
    let quote = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "url = \"{JEV_ENDPOINT}\"\nrequest = \"POST\"\nheader = \"Authorization: Bearer {}\"\nheader = \"Content-Type: application/json\"\ndata-binary = \"{}\"\nwrite-out = \"\\n%{{http_code}}\"\nmax-time = {}\n",
        quote(key.trim()),
        quote(&body.to_string()),
        timeout.as_secs().max(1),
    )
}

/// The JSON body of a curl run whose stdout ends with `\n<http status>`.
fn read_jev_reply(stdout: &[u8], stderr: &[u8], ran: bool) -> Result<Value, OrganizeError> {
    if !ran {
        let detail = String::from_utf8_lossy(stderr);
        return Err(OrganizeError::Jev(format!(
            "request failed: {}",
            detail.trim()
        )));
    }
    let text = String::from_utf8_lossy(stdout);
    let text = text.trim_end();
    let (body, status) = text.rsplit_once('\n').unwrap_or(("", text));
    let status: u16 = status.trim().parse().unwrap_or(0);
    if status != 200 {
        let detail: String = body.chars().take(300).collect();
        return Err(OrganizeError::Jev(format!(
            "HTTP {status}: {}",
            detail.trim()
        )));
    }
    serde_json::from_str(body).map_err(|err| OrganizeError::Jev(format!("unreadable reply: {err}")))
}

/// What [`apply`] did, in the order it did it: enough to [`undo`] it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    /// Document schema.
    pub schema_version: u8,
    /// Sessions created.
    pub created: Vec<CreatedSession>,
    /// Terminals moved.
    pub moved: Vec<MovedPane>,
    /// Sessions renamed.
    pub renamed: Vec<PlannedRename>,
}

impl Journal {
    /// Whether nothing was done.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.created.is_empty() && self.moved.is_empty() && self.renamed.is_empty()
    }
}

/// A session [`apply`] created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedSession {
    /// Its key.
    pub key: SessionKey,
    /// Its name.
    pub name: String,
}

/// A terminal [`apply`] moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovedPane {
    /// The terminal.
    pub pane: ResourceKey,
    /// The session it left.
    pub from: SessionKey,
    /// The session it joined.
    pub to: SessionKey,
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
    /// What did not.
    pub failures: Vec<StepFailure>,
}

/// Carry out `plan`: create its sessions, move its terminals, then rename.
///
/// Moves go in one atomic batch per destination. A step that fails is
/// reported and the rest continue; a move into a session that failed to
/// create is skipped.
pub async fn apply(conn: &mut Connection, plan: &Plan) -> Applied {
    let mut applied = Applied {
        journal: Journal {
            schema_version: JOURNAL_SCHEMA,
            ..Journal::default()
        },
        failures: Vec::new(),
    };
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
    let mut batches: BTreeMap<SessionKey, Vec<&planner::PlannedMove>> = BTreeMap::new();
    for step in &plan.moves {
        let Some(to) = step.to.or_else(|| new_keys.get(&step.to_name).copied()) else {
            applied.failures.push(StepFailure {
                step: format!("move {} to {}", step.label, step.to_name),
                code: "not_found",
                message: format!("session {:?} was not created", step.to_name),
            });
            continue;
        };
        batches.entry(to).or_default().push(step);
    }
    for (to, steps) in batches {
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
                .extend(batch.resources.iter().map(|pane| MovedPane {
                    pane: *pane,
                    from: from[pane],
                    to,
                })),
            Err(err) => applied.fail(format!("move {} terminals", batch.resources.len()), &err),
        }
    }
}

async fn create_session(
    conn: &mut Connection,
    new: &planner::NewSession,
) -> Result<SessionKey, OrgError> {
    let mut attrs = vec![
        (
            phux_core::project_identity::attr::ID.to_owned(),
            new.project_id.clone(),
        ),
        (
            phux_core::project_identity::attr::NAME.to_owned(),
            new.name.clone(),
        ),
    ];
    if let Some(checkout) = &new.checkout {
        attrs.push((
            phux_core::project_identity::attr::CHECKOUT.to_owned(),
            checkout.clone(),
        ));
    }
    attrs.retain(|(_, value)| value.len() <= phux_core::project_identity::MAX_ATTR_BYTES);
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
    let mut applied = Applied::default();
    undo_renames(conn, &inventory, &journal.renamed, &mut applied).await;
    let back = moves_back(&inventory, &journal.moved, &mut applied);
    for (to, panes) in back {
        undo_batch(conn, &inventory, to, &panes, &mut applied).await;
    }
    for created in &journal.created {
        match org::delete_session(conn, created.key).await {
            Ok(_) => applied.journal.created.push(created.clone()),
            Err(err) => applied.fail(format!("delete {}", created.name), &err),
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
        self.failures.push(StepFailure {
            step,
            code: "skipped",
            message: why.to_owned(),
        });
    }
}

/// Where the last organize run's [`Journal`] is kept for this profile.
#[must_use]
pub fn journal_path() -> PathBuf {
    phux_config::instance::state_dir()
        .join("organize")
        .join("last.json")
}

/// Keep `journal` as the one [`undo`] reverses next.
///
/// # Errors
///
/// The file could not be written.
pub fn save_journal(journal: &Journal) -> std::io::Result<()> {
    save_journal_at(&journal_path(), journal)
}

fn save_journal_at(path: &Path, journal: &Journal) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let bytes = serde_json::to_vec_pretty(journal).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}

/// The kept [`Journal`], if there is one.
///
/// # Errors
///
/// The file exists but could not be read or parsed.
pub fn load_journal() -> std::io::Result<Option<Journal>> {
    load_journal_at(&journal_path())
}

fn load_journal_at(path: &Path) -> std::io::Result<Option<Journal>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(std::io::Error::other),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Forget the kept [`Journal`] after it was undone.
///
/// # Errors
///
/// The file exists but could not be removed.
pub fn clear_journal() -> std::io::Result<()> {
    clear_journal_at(&journal_path())
}

fn clear_journal_at(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
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
        let Some(applied) = &self.applied else {
            return if self.plan.is_empty() {
                "organize: every pane is where it belongs".to_owned()
            } else {
                format!("organize: {} moves planned", self.plan.moves.len())
            };
        };
        let journal = &applied.journal;
        let mut parts = vec![count(journal.moved.len(), "pane", "moved")];
        if !journal.created.is_empty() {
            parts.push(count(journal.created.len(), "session", "created"));
        }
        if !journal.renamed.is_empty() {
            parts.push(count(journal.renamed.len(), "session", "renamed"));
        }
        if !applied.failures.is_empty() {
            parts.push(count(applied.failures.len(), "step", "failed"));
        }
        format!(
            "organize ({}): {}",
            self.plan.judge.as_str(),
            parts.join(", ")
        )
    }
}

/// `3 panes moved`, `1 session created`.
#[must_use]
pub fn count(n: usize, noun: &str, verb: &str) -> String {
    let plural = if n == 1 { "" } else { "s" };
    format!("{n} {noun}{plural} {verb}")
}

/// One-line lines describing `plan`, for people.
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
        .filter(|kept| kept.reason == planner::KeptReason::Unsure)
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::testkit::{OrgFixture, ScriptSpec};
    use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
    use phux_protocol::wire::frame::ExecState;

    fn org_spec(fixture: OrgFixture) -> ScriptSpec {
        ScriptSpec::new()
            .server_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
            .organization(fixture)
    }

    #[test]
    fn curl_config_escapes_the_body_and_key() {
        let config = curl_config("k\"ey\n", &serde_json::json!({"a": "b\\c\"d"}), JEV_TIMEOUT);
        assert!(
            config.contains(r#"header = "Authorization: Bearer k\"ey""#),
            "{config}"
        );
        assert!(
            config.contains(r#"data-binary = "{\"a\":\"b\\\\c\\\"d\"}""#),
            "{config}"
        );
    }

    #[test]
    fn jev_replies_carry_their_status() {
        let ok = read_jev_reply(b"{\"answers\":{}}\n200", b"", true).expect("ok");
        assert!(ok.get("answers").is_some());
        let refused = read_jev_reply(b"{\"error\":\"bad key\"}\n401", b"", true).unwrap_err();
        assert!(refused.to_string().contains("HTTP 401"), "{refused}");
        let failed = read_jev_reply(b"", b"curl: (6) no host", false).unwrap_err();
        assert!(failed.to_string().contains("no host"), "{failed}");
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
                checkout: None,
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

    /// A journal survives a write and read, and clearing it twice is fine.
    #[test]
    fn journal_persists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("organize").join("last.json");
        assert_eq!(load_journal_at(&path).expect("read"), None);
        let journal = Journal {
            schema_version: JOURNAL_SCHEMA,
            created: vec![CreatedSession {
                key: SessionKey::new([1; 16]).expect("non-zero"),
                name: "phux".into(),
            }],
            ..Journal::default()
        };
        save_journal_at(&path, &journal).expect("save");
        assert_eq!(load_journal_at(&path).expect("read"), Some(journal));
        clear_journal_at(&path).expect("clear");
        clear_journal_at(&path).expect("clear twice");
    }
}
