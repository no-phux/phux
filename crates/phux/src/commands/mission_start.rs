//! `phux mission start` and `phux mission add --start`.
//!
//! Creates a worktree on `mission/<n>-<slug>`, a session stamped with
//! `mission.task`, and an agent pane. The keeper serves one store request
//! at a time. `commit_start` rechecks the agent cap, so a launch that
//! arrives after the cap filled is refused and nothing is written.

use std::io::{BufRead, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_mission::{DEFAULT_MAX_AGENTS, DEFAULT_RULES, StartDocument};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::OrgFilter;
use phux_server::runtime::default_socket_path;
use phux_work::{
    Alias, Client, Objective, OperationId, StartApplied, StartBinding, StartPlan, StartRequest,
};

use super::json_err::codes;
use super::mission::{self, OpOpt};

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
    let (kind, model) = match phux_mission::parse_agent(&agent_text) {
        Ok(parsed) => parsed,
        Err(err) => return mission::report(json, &err),
    };
    let paths = phux_mission::task_paths(
        &state_dir,
        &objective.repo,
        objective.alias.number,
        &objective.title,
    );
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
        },
    )
}

struct Place<'a> {
    json: bool,
    added: bool,
    objective: &'a Objective,
    root: &'a Path,
    settings: &'a Settings,
    paths: phux_mission::TaskPaths,
    kind: String,
    model: Option<String>,
    socket: Option<PathBuf>,
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
    } = place;
    let from = match origin_head(root) {
        Ok(from) => from,
        Err(err) => return failed(json, added, &objective.alias, &err),
    };
    if let Err(err) = std::fs::create_dir_all(&settings.target_dir) {
        return failed(
            json,
            added,
            &objective.alias,
            &format!("could not create {}: {err}", settings.target_dir.display()),
        );
    }
    let brief = phux_mission::brief_markdown(
        objective.alias.number,
        &objective.alias.display(),
        &objective.repo,
        &objective.title,
        &objective.description,
        &settings.rules,
    );
    if let Err(err) = write_brief(&paths.brief, &brief) {
        return failed(json, added, &objective.alias, &err);
    }
    let socket = socket.unwrap_or_else(default_socket_path);
    let Ok(created) = super::worktree::create_task_worktree(
        root,
        &paths.branch,
        &paths.worktree,
        &from,
        &socket,
        &objective.id.to_hex(),
    ) else {
        return cleanup(
            json,
            added,
            &objective.alias,
            root,
            &paths.worktree,
            &paths.branch,
            "could not create the worktree session",
        );
    };
    let binding = match worker_binding(
        &socket,
        &created.session,
        &kind,
        &paths,
        model.as_deref(),
        &settings.target_dir,
        settings.sccache.as_deref(),
        objective,
    ) {
        Ok(binding) => binding,
        Err(err) => {
            return cleanup(
                json,
                added,
                &objective.alias,
                root,
                &paths.worktree,
                &paths.branch,
                &err,
            );
        }
    };
    request.launch = Some(binding);
    match store.commit_start(request) {
        Ok(applied) => emit(json, &applied),
        Err(err) => cleanup(
            json,
            added,
            &objective.alias,
            root,
            &paths.worktree,
            &paths.branch,
            &err.to_string(),
        ),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the worker binding is one value per launch input"
)]
fn worker_binding(
    socket: &Path,
    session: &str,
    kind: &str,
    paths: &phux_mission::TaskPaths,
    model: Option<&str>,
    target_dir: &Path,
    sccache: Option<&str>,
    objective: &Objective,
) -> Result<StartBinding, String> {
    let credential = mint_credential(socket, session)?;
    let title = phux_mission::tab_title(objective.alias.number, &objective.title);
    let launched = super::launch::launch_in_session(&super::launch::SessionLaunch {
        kind: kind.to_owned(),
        extra: phux_mission::brief_argv(kind, &paths.brief, model),
        cwd: paths.worktree.clone(),
        socket: socket.to_path_buf(),
        session: session.to_owned(),
        env: phux_mission::build_env(target_dir, sccache),
        credential_socket: Some(credential),
    })?;
    let session_key = rename_worker(socket, session, launched.terminal_id, &title)?;
    Ok(StartBinding {
        session_key,
        session: session.to_owned(),
        terminal_id: launched.terminal_id.to_string(),
        worktree_path: paths.worktree.display().to_string(),
        branch: paths.branch.clone(),
        tab_title: title,
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

fn cleanup(
    json: bool,
    added: bool,
    alias: &Alias,
    repo: &Path,
    worktree: &Path,
    branch: &str,
    reason: &str,
) -> ExitCode {
    let message = prefixed(added, alias, reason);
    if !confirm_remove(worktree) {
        return mission::report_code(
            json,
            codes::MISSION_CONFIRMATION_REQUIRED,
            &format!("{message}; worktree kept at {}", worktree.display()),
            "remove the worktree from a terminal if it should go",
            2,
        );
    }
    let dest = worktree.to_string_lossy().into_owned();
    let _ = super::git::git_bytes(repo, &["worktree", "remove", "--force", &dest]);
    let _ = super::git::git_bytes(repo, &["branch", "-D", branch]);
    mission::report_code(
        json,
        codes::MISSION_START_FAILED,
        &message,
        "the worktree was removed; retry the start",
        1,
    )
}

fn confirm_remove(worktree: &Path) -> bool {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return false;
    }
    eprint!(
        "phux: remove the worktree at {}? [y/N] ",
        worktree.display()
    );
    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line).unwrap_or(0);
    read > 0 && matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn prefixed(added: bool, alias: &Alias, message: &str) -> String {
    if added {
        format!("added {}; {message}", alias.display())
    } else {
        message.to_owned()
    }
}

struct Settings {
    max_agents: u32,
    target_dir: PathBuf,
    sccache: Option<String>,
    agent: Option<String>,
    rules: String,
}

fn load_settings(repo: &Path, state_dir: &Path) -> Result<Settings, String> {
    let mut settings = Settings {
        max_agents: DEFAULT_MAX_AGENTS,
        target_dir: phux_mission::default_target_dir(state_dir),
        sccache: None,
        agent: None,
        rules: DEFAULT_RULES.to_owned(),
    };
    let path = repo.join(".phux").join("mission.toml");
    if !path.exists() {
        return Ok(settings);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("could not read {}: {err}", path.display()))?;
    let value: toml::Value = toml::from_str(&text)
        .map_err(|err| format!("{} is not valid toml: {err}", path.display()))?;
    let Some(mission) = value.get("mission") else {
        return Ok(settings);
    };
    if let Some(max) = mission.get("max_agents") {
        let Some(max) = max.as_integer().and_then(|max| u32::try_from(max).ok()) else {
            return Err("[mission] max_agents must be an integer from 1 up".to_owned());
        };
        if max == 0 {
            return Err("[mission] max_agents must be at least 1".to_owned());
        }
        settings.max_agents = max;
    }
    if let Some(dir) = mission
        .get("shared_target_dir")
        .and_then(toml::Value::as_str)
    {
        let dir = PathBuf::from(dir);
        settings.target_dir = if dir.is_absolute() {
            dir
        } else {
            repo.join(dir)
        };
    }
    match mission.get("sccache") {
        None | Some(toml::Value::Boolean(false)) => {}
        Some(toml::Value::Boolean(true)) => settings.sccache = Some("sccache".to_owned()),
        Some(toml::Value::String(bin)) if !bin.is_empty() => settings.sccache = Some(bin.clone()),
        Some(_) => {
            return Err("[mission] sccache must be true or a program name".to_owned());
        }
    }
    if let Some(agent) = mission.get("agent").and_then(toml::Value::as_str) {
        settings.agent = Some(agent.to_owned());
    }
    if let Some(rules) = mission.get("rules").and_then(toml::Value::as_str) {
        settings.rules = if rules.ends_with('\n') {
            rules.to_owned()
        } else {
            format!("{rules}\n")
        };
    }
    Ok(settings)
}

fn repo_root() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|err| err.to_string())?;
    let root = super::git::git_text(&cwd, &["rev-parse", "--show-toplevel"])?;
    PathBuf::from(root.trim())
        .canonicalize()
        .map_err(|err| format!("could not canonicalize the git worktree: {err}"))
}

fn origin_head(root: &Path) -> Result<String, String> {
    let text = super::git::git_text(root, &["rev-parse", "--abbrev-ref", "origin/HEAD"])?;
    let text = text.trim();
    if text.is_empty() || text == "origin/HEAD" {
        return Err(
            "origin has no default branch; set it with `git remote set-head origin <branch>`"
                .to_owned(),
        );
    }
    Ok(text.to_owned())
}

fn write_brief(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }
    std::fs::write(path, body).map_err(|err| format!("could not write {}: {err}", path.display()))
}

fn mint_credential(socket: &Path, session: &str) -> Result<String, String> {
    let rt = super::cli_runtime().map_err(|_| "could not build a runtime".to_owned())?;
    rt.block_on(async {
        let mut conn = super::server_target::ServerTarget::local(socket)
            .connect()
            .await
            .map_err(|err| err.to_string())?;
        if !phux_client::org::supported(&conn) {
            return Err("this server cannot mint an agent credential".to_owned());
        }
        let inventory = phux_client::org::fetch(&mut conn, OrgFilter::All, 1)
            .await
            .map_err(|err| err.to_string())?;
        let record = inventory
            .session_named(session)
            .ok_or_else(|| format!("session {session} has no record"))?;
        let delegation = phux_client::delegation::delegate_agent(
            &mut conn,
            2,
            phux_client::org::mint_operation(),
            record.key,
            0,
        )
        .await
        .map_err(|err| format!("could not mint an agent credential: {err}"))?;
        drop(conn);
        Ok(delegation.socket_path)
    })
}

fn rename_worker(socket: &Path, session: &str, worker: u64, title: &str) -> Result<String, String> {
    let id = u32::try_from(worker).map_err(|_| format!("pane id {worker} does not fit"))?;
    let live = ResourceId::local(id);
    let rt = super::cli_runtime().map_err(|_| "could not build a runtime".to_owned())?;
    rt.block_on(async {
        let mut conn = super::server_target::ServerTarget::local(socket)
            .connect()
            .await
            .map_err(|err| err.to_string())?;
        let inventory = phux_client::org::fetch(&mut conn, OrgFilter::All, 3)
            .await
            .map_err(|err| err.to_string())?;
        let record = inventory
            .session_named(session)
            .ok_or_else(|| format!("session {session} has no record"))?;
        let session_key = record.key.to_string();
        let resource_key = inventory
            .resource_by_live(&live)
            .ok_or_else(|| format!("pane {worker} has no record to label"))?
            .key;
        phux_client::org::rename_resource(&mut conn, resource_key, Some(title))
            .await
            .map_err(|err| format!("could not title the worker pane: {err}"))?;
        drop(conn);
        Ok(session_key)
    })
}

/// Child operation id for `add --start`.
#[must_use]
pub(crate) fn start_operation(add: OperationId) -> OperationId {
    add.derive(START_TAG)
}
