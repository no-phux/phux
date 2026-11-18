//! `phux mission start` and `phux mission add --start`.
//!
//! Creates a worktree on `mission/<n>-<slug>`, a session stamped with
//! `mission.task`, and an agent pane. The keeper serves one store request
//! at a time. `commit_start` rechecks the agent cap, so a launch that
//! arrives after the cap filled is refused and nothing is written.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::attach::connection::Connection;
use phux_config::socket::default_socket_path;
use phux_protocol::api::{TerminalCreate, TerminalCreated, TerminalOperation};
use phux_protocol::ids::{AuthorityId, SessionKey};
use phux_protocol::wire::frame::OrgFilter;
use phux_work::mission::{
    StartDocument, TaskPaths, brief_argv, brief_markdown, build_env, parse_agent, tab_title,
    task_paths,
};
use phux_work::{
    Alias, Client, LaunchQueued, Objective, OpenRun, OperationId, StartApplied, StartBinding,
    StartPlan, StartRequest,
};

use super::json_err::codes;
use super::mission::{self, OpOpt};

mod settings;
pub(super) use settings::configured_max_agents;
use settings::{Settings, load_settings, origin_head, repo_root, write_brief};

/// Tag mixed into the add operation id for the follow-up start.
const START_TAG: u8 = 0xA5;

pub(crate) fn start_new(
    store: &Client,
    json: bool,
    objective: &Objective,
    agent: Option<&str>,
    operation_id: OperationId,
    socket: Option<PathBuf>,
) -> ExitCode {
    start_task(
        store,
        json,
        objective,
        agent,
        objective.revision,
        operation_id,
        socket,
        true,
    )
}

pub(crate) fn start_existing(
    store: &Client,
    json: bool,
    alias: &Alias,
    agent: Option<&str>,
    expected_revision: u64,
    op: &OpOpt,
    socket: Option<PathBuf>,
) -> ExitCode {
    let operation_id = match mission::operation_id(json, op) {
        Ok(id) => id,
        Err(code) => return code,
    };
    let objective = match store.show(alias.clone()) {
        Ok(objective) => objective,
        Err(err) => return mission::report(json, &err),
    };
    start_task(
        store,
        json,
        &objective,
        agent,
        expected_revision,
        operation_id,
        socket,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn start_task(
    store: &Client,
    json: bool,
    objective: &Objective,
    agent: Option<&str>,
    expected_revision: u64,
    operation_id: OperationId,
    socket: Option<PathBuf>,
    added: bool,
) -> ExitCode {
    let socket = socket.unwrap_or_else(default_socket_path);
    let root = match repo_root() {
        Ok(root) => root,
        Err(err) => return failed(json, added, &objective.alias, &err),
    };
    let state_dir = phux_config::instance::state_dir();
    let settings = match load_settings(&root, &state_dir) {
        Ok(settings) => settings,
        Err(err) => return failed(json, added, &objective.alias, &err),
    };
    let Some(agent_text) = agent.map(str::to_owned).or_else(|| settings.agent.clone()) else {
        return failed(
            json,
            added,
            &objective.alias,
            "pass --agent KIND[/MODEL] or set [mission] agent",
        );
    };
    let (kind, model) = match parse_agent(&agent_text) {
        Ok(parsed) => parsed,
        Err(err) => return mission::report(json, &err),
    };
    let paths = task_paths(
        &state_dir,
        &objective.repo,
        objective.alias.number,
        &objective.title,
    );
    let authority = match ensure_owner(&socket) {
        Ok(authority) => authority,
        Err(error) => return failed(json, added, &objective.alias, &error),
    };
    let request = StartRequest {
        alias: objective.alias.clone(),
        agent_kind: kind.clone(),
        model: model.clone().unwrap_or_default(),
        max_agents: settings.max_agents,
        expected_revision,
        operation_id,
        actor: mission::actor(),
        worktree_path: paths.worktree.display().to_string(),
        branch: paths.branch.clone(),
        repo_root: root.display().to_string(),
        authority: Some(authority),
        launch: None,
    };
    match store.replay_start(&request) {
        Ok(Some(applied)) => return emit(json, &applied),
        Ok(None) => {}
        Err(err) => return mission::report(json, &err),
    }
    let plan = match store.plan_start(&request) {
        Ok(plan) => plan,
        Err(err) => return mission::report(json, &err),
    };
    if matches!(plan, StartPlan::Queue { .. }) {
        return match store.commit_start(request) {
            Ok(applied) => emit(json, &applied),
            Err(err) => mission::report(json, &err),
        };
    }
    place_agent(
        store,
        request,
        Place {
            json,
            added,
            objective,
            root: &root,
            settings: &settings,
            paths,
            kind,
            model,
            socket,
            authority,
        },
    )
}

struct Place<'a> {
    json: bool,
    added: bool,
    objective: &'a Objective,
    root: &'a Path,
    settings: &'a Settings,
    paths: TaskPaths,
    kind: String,
    model: Option<String>,
    socket: PathBuf,
    authority: AuthorityId,
}

fn place_agent(store: &Client, mut request: StartRequest, place: Place<'_>) -> ExitCode {
    let Place {
        json,
        added,
        objective,
        root,
        settings,
        paths,
        kind,
        model,
        socket,
        authority,
    } = place;
    let binding = match prepare_worker(&Prepared {
        root,
        objective,
        settings,
        paths: &paths,
        kind: &kind,
        model: model.as_deref(),
        socket: &socket,
        operation: request.operation_id,
        authority,
    }) {
        Ok(binding) => binding,
        Err(PrepareError::Early(err)) => return failed(json, added, &objective.alias, &err),
        Err(PrepareError::Launched(err)) => {
            return keep_worktree(json, added, &objective.alias, &paths.worktree, &err);
        }
    };
    request.launch = Some(binding);
    let operation = request.operation_id.to_hex();
    match store.commit_start(request) {
        Ok(applied) => emit(json, &applied),
        Err(err) => keep_worktree(
            json,
            added,
            &objective.alias,
            &paths.worktree,
            &format!("worker operation {operation}: keeper did not confirm the binding: {err}"),
        ),
    }
}

struct Prepared<'a> {
    root: &'a Path,
    objective: &'a Objective,
    settings: &'a Settings,
    paths: &'a TaskPaths,
    kind: &'a str,
    model: Option<&'a str>,
    socket: &'a Path,
    operation: OperationId,
    authority: AuthorityId,
}

enum PrepareError {
    Early(String),
    Launched(String),
}

fn prepare_worker(place: &Prepared<'_>) -> Result<StartBinding, PrepareError> {
    match place.paths.worktree.symlink_metadata() {
        Ok(_) => {
            confirm_recovery_path(place).map_err(PrepareError::Launched)?;
            let session = super::worktree::session_name_for(&place.paths.worktree);
            return worker_binding(place, &session, false).map_err(PrepareError::Launched);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(PrepareError::Early(error.to_string())),
    }
    let from = origin_head(place.root).map_err(PrepareError::Early)?;
    std::fs::create_dir_all(&place.settings.target_dir).map_err(|err| {
        PrepareError::Early(format!(
            "could not create {}: {err}",
            place.settings.target_dir.display()
        ))
    })?;
    let brief = brief_markdown(
        place.objective.alias.number,
        &place.objective.alias.display(),
        &place.objective.repo,
        &place.objective.title,
        &place.objective.description,
        &place.settings.rules,
    );
    write_brief(&place.paths.brief, &brief).map_err(PrepareError::Early)?;
    let created = super::worktree::create_task_worktree(
        place.root,
        &place.paths.branch,
        &place.paths.worktree,
        &from,
        place.socket,
        &place.objective.id.to_hex(),
        place.authority,
    )
    .map_err(|_| PrepareError::Launched("could not create the worktree session".to_owned()))?;
    worker_binding(place, &created.session, true).map_err(PrepareError::Launched)
}

fn confirm_recovery_path(place: &Prepared<'_>) -> Result<(), String> {
    let input = super::git::git_bytes(place.root, &["worktree", "list", "--porcelain", "-z"])?;
    let path = place
        .paths
        .worktree
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let matches = super::git::parse_worktrees(&input, place.root)?
        .into_iter()
        .any(|entry| {
            !entry.is_main
                && entry.branch.as_deref() == Some(place.paths.branch.as_str())
                && entry
                    .path
                    .canonicalize()
                    .is_ok_and(|candidate| candidate == path)
        });
    if matches {
        Ok(())
    } else {
        Err("existing path is not this mission's registered worktree and branch".to_owned())
    }
}

/// Launch the oldest queued run through the same worktree and brief path as
/// `mission start`, then admit it. A repeated `operation_id` does not spawn.
pub(crate) fn promote_queued(
    store: &Client,
    run: &OpenRun,
    max_agents: u32,
    operation_id: OperationId,
    socket: &Path,
) -> Result<(), String> {
    if store
        .replay_launch(operation_id)
        .map_err(|err| err.to_string())?
        .is_some()
    {
        return Ok(());
    }
    let owner = run.authority.ok_or("queued run has no recorded owner")?;
    if owner != current_owner(socket)? {
        return Err(format!("queued run belongs to authority {owner}"));
    }
    let objective = store
        .show(run.alias.clone())
        .map_err(|err| err.to_string())?;
    let root = PathBuf::from(&run.repo_root);
    if root.as_os_str().is_empty() {
        return Err(format!(
            "{} has no recorded repository root",
            run.alias.display()
        ));
    }
    let state_dir = phux_config::instance::state_dir();
    let settings = load_settings(&root, &state_dir)?;
    let mut paths = task_paths(
        &state_dir,
        &objective.repo,
        objective.alias.number,
        &objective.title,
    );
    if !run.worktree_path.is_empty() {
        paths.worktree = PathBuf::from(&run.worktree_path);
    }
    if !run.branch.is_empty() {
        paths.branch.clone_from(&run.branch);
    }
    let model = if run.model.is_empty() {
        None
    } else {
        Some(run.model.as_str())
    };
    let binding = match prepare_worker(&Prepared {
        root: &root,
        objective: &objective,
        settings: &settings,
        paths: &paths,
        kind: &run.agent_kind,
        model,
        socket,
        operation: operation_id,
        authority: owner,
    }) {
        Ok(binding) => binding,
        Err(PrepareError::Early(err)) => return Err(err),
        Err(PrepareError::Launched(err)) => return Err(err),
    };
    let admitted = store.launch_queued(LaunchQueued {
        alias: run.alias.clone(),
        expected_revision: objective.revision,
        operation_id,
        actor: ACTOR_KEEPER.to_owned(),
        max_agents,
        launch: binding,
    });
    admitted.map(|_| ()).map_err(|err| {
        format!(
            "worker operation {}: keeper did not confirm the binding: {err}",
            operation_id.to_hex()
        )
    })
}

const ACTOR_KEEPER: &str = "keeper";

fn worker_binding(
    place: &Prepared<'_>,
    session: &str,
    may_launch: bool,
) -> Result<StartBinding, String> {
    let title = tab_title(place.objective.alias.number, &place.objective.title);
    let rt = super::cli_runtime().map_err(|_| "could not build a runtime".to_owned())?;
    let created = rt.block_on(launch_worker(place, session, &title, may_launch))?;
    let execution = created.execution_ref.ok_or_else(|| {
        "worker launch committed but exact execution is unavailable; reconcile its receipt"
            .to_owned()
    })?;
    Ok(StartBinding {
        session_key: created.session,
        session: session.to_owned(),
        terminal_id: created.terminal_id.map(|id| id.to_string()),
        execution: Some(execution),
        worktree_path: place.paths.worktree.display().to_string(),
        branch: place.paths.branch.clone(),
        tab_title: title,
    })
}

fn worker_recipe(
    kind: &str,
    paths: &TaskPaths,
    model: Option<&str>,
) -> Result<(Vec<String>, Option<super::agent::PreparedAgentSession>), String> {
    let config = phux_config::loader::config_path();
    let mut error = format!("no launch integration for {kind}");
    for id in phux_work::mission::integration_ids(kind) {
        let resolved = match phux_plugin::resolve_launch(
            &config,
            id,
            &brief_argv(id, &paths.brief, model),
            &paths.worktree,
            phux_plugin::LaunchMode::Terminal,
        ) {
            Ok(resolved) => resolved,
            Err(err) => {
                error = err.to_string();
                continue;
            }
        };
        let prepared = super::agent::prepare_for_launch(&resolved)?;
        let argv = prepared.as_ref().map_or(resolved.argv, |p| p.argv.clone());
        return Ok((argv, prepared));
    }
    Err(error)
}

async fn launch_worker(
    place: &Prepared<'_>,
    name: &str,
    label: &str,
    may_launch: bool,
) -> Result<TerminalCreated, String> {
    let mut conn = super::server_target::ServerTarget::local(place.socket)
        .connect()
        .await
        .map_err(|error| error.to_string())?;
    let inventory = phux_client::org::fetch(&mut conn, OrgFilter::All, 1)
        .await
        .map_err(|error| error.to_string())?;
    if inventory.authority != place.authority {
        return Err(format!(
            "worker launch belongs to authority {}",
            place.authority
        ));
    }
    let task = place.objective.id.to_hex();
    let session = inventory
        .session_named(name)
        .filter(|session| {
            session
                .attrs
                .iter()
                .any(|(key, value)| key == "mission.task" && value == &task)
        })
        .ok_or_else(|| format!("no session {name} bound to objective {task}"))?;
    let key = place.operation.to_hex();
    let lookup = TerminalOperation {
        session: session.key.to_string(),
        idempotency_key: key.clone(),
    };
    match phux_client::terminal::operation(&mut conn, &lookup).await {
        Ok(receipt) => return Ok(receipt),
        Err(phux_client::terminal::CreateError::NotRetained) if may_launch => {
            submit_worker(&mut conn, place, session.key, label, key).await
        }
        Err(phux_client::terminal::CreateError::NotRetained) => Err(format!(
            "worker receipt {key} is not retained for the existing worktree; recovery did not start another process"
        )),
        Err(error) => Err(error.to_string()),
    }
}

fn ensure_owner(socket: &Path) -> Result<AuthorityId, String> {
    super::server::ensure_server_unseeded(socket, false).map_err(|error| error.to_string())?;
    current_owner(socket)
}

fn current_owner(socket: &Path) -> Result<AuthorityId, String> {
    let rt = super::cli_runtime().map_err(|_| "could not build a runtime".to_owned())?;
    rt.block_on(async {
        let mut conn = Connection::connect(socket)
            .await
            .map_err(|error| error.to_string())?;
        owner_on(&mut conn).await
    })
}

pub(super) async fn owner_on(conn: &mut Connection) -> Result<AuthorityId, String> {
    // This exact organization read is bounded even on a large authority.
    let key =
        phux_protocol::ids::ResourceKey::new([0xff; 16]).ok_or("invalid authority probe key")?;
    phux_client::resource::show::show_key_level_on(conn, key)
        .await
        .map(|(authority, _)| authority)
        .map_err(|error| error.to_string())
}

async fn submit_worker(
    conn: &mut Connection,
    place: &Prepared<'_>,
    session: SessionKey,
    label: &str,
    key: String,
) -> Result<TerminalCreated, String> {
    let (argv, provenance) = worker_recipe(place.kind, place.paths, place.model)?;
    let cwd = place
        .paths
        .worktree
        .to_str()
        .ok_or("worktree path is not UTF-8")?;
    let delegation = phux_client::delegation::delegate_agent(
        conn,
        2,
        phux_client::org::mint_operation(),
        session,
        0,
    )
    .await
    .map_err(|error| format!("could not mint an agent credential: {error}"))?;
    let mut request = TerminalCreate::new(session.to_string(), key);
    request.cwd = Some(cwd.to_owned());
    request.name = Some(label.to_owned());
    request.env.extend(build_env(
        &place.settings.target_dir,
        place.settings.sccache.as_deref(),
    ));
    // The public env map cannot override server routing. This existing
    // delegated socket is injected into the worker argv, not the primary socket.
    request.argv.extend([
        "/usr/bin/env".to_owned(),
        format!("PHUX_SOCKET={}", delegation.socket_path),
    ]);
    if let Some(provenance) = &provenance {
        request.argv.extend(
            provenance
                .env
                .iter()
                .filter(|(name, _)| !request.env.contains_key(*name))
                .map(|(name, value)| format!("{name}={value}")),
        );
    }
    request.argv.extend(argv);
    phux_client::terminal::create_with_provenance(
        conn,
        &request,
        provenance.as_ref().map(|prepared| &prepared.record),
    )
    .await
    .map_err(|error| {
        format!(
            "mission worker operation {}: {error}",
            request.idempotency_key
        )
    })
}

fn emit(json: bool, applied: &StartApplied) -> ExitCode {
    if json {
        return crate::output::json(&StartDocument::from_applied(applied));
    }
    let alias = applied.objective.alias.display();
    let rev = applied.objective.revision;
    if applied.disposition == "queued" {
        match applied.queue_position {
            Some(position) => outln!("queued {alias} (rev {rev}), position {position}"),
            None => outln!("queued {alias} (rev {rev})"),
        }
    } else {
        outln!("started {alias} (rev {rev})");
        if let Some(binding) = &applied.binding {
            outln!("session {}", binding.session);
            outln!("worktree {}", binding.worktree_path);
        }
    }
    ExitCode::SUCCESS
}

fn failed(json: bool, added: bool, alias: &Alias, message: &str) -> ExitCode {
    mission::report_code(
        json,
        codes::MISSION_START_FAILED,
        &prefixed(added, alias, message),
        "fix the problem named above and retry",
        1,
    )
}

fn keep_worktree(
    json: bool,
    added: bool,
    alias: &Alias,
    worktree: &Path,
    reason: &str,
) -> ExitCode {
    let message = prefixed(added, alias, reason);
    mission::report_code(
        json,
        codes::MISSION_START_FAILED,
        &format!("{message}; worktree kept at {}", worktree.display()),
        "reconcile the original operation and inspect the worker before retrying",
        1,
    )
}

fn prefixed(added: bool, alias: &Alias, message: &str) -> String {
    if added {
        format!("added {}; {message}", alias.display())
    } else {
        message.to_owned()
    }
}

/// Child operation id for `add --start`.
#[must_use]
pub(crate) fn start_operation(add: OperationId) -> OperationId {
    add.derive(START_TAG)
}
