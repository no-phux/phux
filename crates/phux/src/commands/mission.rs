//! `phux mission`: tasks in the local work store.
//!
//! `list`, `show`, `edit`, `archive`, `restore`, and `keeper` call the
//! store's keeper and do not contact a phux server. `start` and
//! `add --start` also dial the phux server to create the worktree session.

use std::fmt::Write as _;
use std::process::ExitCode;
use std::time::Duration;

use phux_mission::{ListDocument, MutationDocument, SCHEMA, SCHEMA_VERSION, ShowDocument, Verb};
use phux_work::{
    AddObjective, Alias, Client, EditObjective, Endpoint, KeeperExit, Launcher, ListFilter,
    OperationId, RevisionEdit,
};

use super::json_err::codes;

/// A keeper with no request for this long exits; the next verb starts another.
const KEEPER_IDLE_EXIT: Duration = Duration::from_secs(600);
/// Overrides [`KEEPER_IDLE_EXIT`] for a keeper started without `--idle-exit`.
const KEEPER_IDLE_ENV: &str = "PHUX_WORK_IDLE_EXIT_SECS";

#[derive(Debug, usage::Subcommands)]
pub(crate) enum MissionAction {
    /// Add a task and print its alias
    Add {
        /// Task title.
        #[usage(value_name = "TITLE")]
        title: String,
        /// Description stored with the task.
        #[usage(long)]
        description: Option<String>,
        /// Repo id the alias is allocated under, such as `phux` for `phux/3`.
        /// Defaults to the origin remote, or the current directory's name when
        /// there is no remote.
        #[usage(long, value_name = "ID")]
        repo: Option<String>,
        /// Start the new task in its own worktree session.
        #[usage(long)]
        start: bool,
        /// Agent for `--start`, as KIND or KIND/MODEL.
        #[usage(long, value_name = "KIND")]
        agent: Option<String>,
        #[usage(flatten)]
        op: OpOpt,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// Start a task in its own worktree session with an agent
    Start {
        /// Task alias in `repo/n` form, such as `phux/3`.
        #[usage(value_name = "ALIAS")]
        alias: String,
        /// Agent as KIND or KIND/MODEL. Defaults to `[mission] agent`.
        #[usage(long, value_name = "KIND")]
        agent: Option<String>,
        #[usage(flatten)]
        rev: RevOpt,
        #[usage(flatten)]
        op: OpOpt,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// List tasks
    List {
        /// Only tasks for this repo id.
        #[usage(long, value_name = "ID")]
        repo: Option<String>,
        /// List archived tasks instead of active ones.
        #[usage(long)]
        archived: bool,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// Show one task
    Show {
        /// Task alias in `repo/n` form, such as `phux/3`.
        #[usage(value_name = "ALIAS")]
        alias: String,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// Change a task's title or description
    Edit {
        /// Task alias in `repo/n` form, such as `phux/3`.
        #[usage(value_name = "ALIAS")]
        alias: String,
        /// New title.
        #[usage(long)]
        title: Option<String>,
        /// New description.
        #[usage(long)]
        description: Option<String>,
        #[usage(flatten)]
        rev: RevOpt,
        #[usage(flatten)]
        op: OpOpt,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// Archive a task
    Archive {
        /// Task alias in `repo/n` form, such as `phux/3`.
        #[usage(value_name = "ALIAS")]
        alias: String,
        #[usage(flatten)]
        rev: RevOpt,
        #[usage(flatten)]
        op: OpOpt,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// Restore an archived task
    Restore {
        /// Task alias in `repo/n` form, such as `phux/3`.
        #[usage(value_name = "ALIAS")]
        alias: String,
        #[usage(flatten)]
        rev: RevOpt,
        #[usage(flatten)]
        op: OpOpt,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// Run the keeper that writes the work store
    ///
    /// The one process that opens this state directory's work store. The
    /// other verbs start it on demand, so it rarely needs running by hand.
    /// It exits after `--idle-exit` seconds without a request.
    Keeper {
        /// Seconds without a request before exiting. Default 600.
        #[usage(long, value_name = "SECS")]
        idle_exit: Option<u64>,
    },
}

#[derive(Debug, usage::Args)]
pub(crate) struct OpOpt {
    /// Repeat a write safely: 32 hex digits. The same id returns the first result.
    #[usage(long, value_name = "HEX32")]
    operation_id: Option<String>,
}

#[derive(Debug, usage::Args)]
pub(crate) struct RevOpt {
    /// Required. The task's current revision; the write is refused when it differs.
    #[usage(long, value_name = "N")]
    expected_revision: Option<u64>,
}

pub(crate) fn run(action: Option<MissionAction>, socket: Option<std::path::PathBuf>) -> ExitCode {
    action.map_or_else(print_help, |action| run_action(action, socket))
}

fn print_help() -> ExitCode {
    let spec = crate::Cli::spec();
    let Some(mission) = spec
        .root
        .subcommands
        .iter()
        .find(|sub| sub.cmd.name == "mission")
    else {
        return ExitCode::from(2);
    };
    let Some(page) = crate::render_help_page(mission.cmd, true, usage::help::Style::PLAIN) else {
        return ExitCode::from(2);
    };
    outln!("{page}");
    ExitCode::SUCCESS
}

fn run_action(action: MissionAction, socket: Option<std::path::PathBuf>) -> ExitCode {
    if let MissionAction::Keeper { idle_exit } = action {
        return keeper(idle_exit);
    }
    let json = wants_json(&action);
    let store = match client(json) {
        Ok(client) => client,
        Err(code) => return code,
    };
    match action {
        MissionAction::Add {
            title,
            description,
            repo,
            start,
            agent,
            op,
            ..
        } => add(
            &store,
            json,
            &title,
            description.as_deref(),
            repo.as_deref(),
            start,
            agent.as_deref(),
            &op,
            socket,
        ),
        MissionAction::Start {
            alias,
            agent,
            rev,
            op,
            ..
        } => {
            let alias = match parse_alias(json, &alias) {
                Ok(alias) => alias,
                Err(code) => return code,
            };
            let expected = match require_expected(&store, json, &alias, rev.expected_revision) {
                Ok(expected) => expected,
                Err(code) => return code,
            };
            super::mission_start::start_existing(
                &store,
                json,
                &alias,
                agent.as_deref(),
                expected,
                &op,
                socket,
            )
        }
        MissionAction::List { repo, archived, .. } => list(&store, json, repo.as_deref(), archived),
        MissionAction::Show { alias, .. } => show(&store, json, &alias),
        MissionAction::Edit {
            alias,
            title,
            description,
            rev,
            op,
            ..
        } => edit(
            &store,
            json,
            &alias,
            title.as_deref(),
            description.as_deref(),
            &rev,
            &op,
        ),
        MissionAction::Archive { alias, rev, op, .. } => {
            set_archived(&store, json, &alias, &rev, &op, true)
        }
        MissionAction::Restore { alias, rev, op, .. } => {
            set_archived(&store, json, &alias, &rev, &op, false)
        }
        MissionAction::Keeper { .. } => unreachable!("handled above"),
    }
}

const fn wants_json(action: &MissionAction) -> bool {
    match action {
        MissionAction::Add { json, .. }
        | MissionAction::Start { json, .. }
        | MissionAction::List { json, .. }
        | MissionAction::Show { json, .. }
        | MissionAction::Edit { json, .. }
        | MissionAction::Archive { json, .. }
        | MissionAction::Restore { json, .. } => json.json,
        MissionAction::Keeper { .. } => false,
    }
}

fn endpoint() -> Endpoint {
    Endpoint::new(
        &phux_config::instance::state_dir(),
        &phux_config::instance::runtime_dir(),
    )
}

fn client(json: bool) -> Result<Client, ExitCode> {
    let endpoint = endpoint();
    let program = phux_config::instance::running_executable().map_err(|err| {
        fail(
            json,
            &phux_work::Error::Keeper(format!("could not find this phux binary: {err}")),
        )
    })?;
    let log = endpoint.store.parent().map(|dir| dir.join("keeper.log"));
    let launcher = Launcher {
        program,
        args: vec!["mission".into(), "keeper".into()],
        log,
    };
    Ok(Client::new(endpoint, Some(launcher)))
}

fn keeper(idle_exit: Option<u64>) -> ExitCode {
    let idle = idle_exit
        .or_else(|| std::env::var(KEEPER_IDLE_ENV).ok()?.parse().ok())
        .map_or(KEEPER_IDLE_EXIT, Duration::from_secs);
    match phux_work::run_keeper(&endpoint(), idle) {
        Ok(KeeperExit::StoreFailed) => ExitCode::from(1),
        Ok(KeeperExit::Idle | KeeperExit::AlreadyRunning | KeeperExit::Stale) => ExitCode::SUCCESS,
        Err(err) => {
            error!("{err}");
            ExitCode::from(1)
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "one argument per `mission add` input, including the optional start"
)]
fn add(
    store: &Client,
    json: bool,
    title: &str,
    description: Option<&str>,
    repo: Option<&str>,
    start: bool,
    agent: Option<&str>,
    op: &OpOpt,
    socket: Option<std::path::PathBuf>,
) -> ExitCode {
    if agent.is_some() && !start {
        return usage(
            json,
            "--agent requires --start",
            "pass --start, or drop --agent",
        );
    }
    let repo = match repo.map_or_else(|| default_repo(json), |repo| Ok(repo.to_owned())) {
        Ok(repo) => repo,
        Err(code) => return code,
    };
    let operation_id = match operation_id(json, op) {
        Ok(id) => id,
        Err(code) => return code,
    };
    match store.add(AddObjective {
        repo,
        title: title.to_owned(),
        description: description.unwrap_or("").to_owned(),
        operation_id,
        actor: actor(),
    }) {
        Ok(applied) if start => super::mission_start::start_new(
            store,
            json,
            &applied.objective,
            agent,
            super::mission_start::start_operation(applied.operation_id),
            socket,
        ),
        Ok(applied) => emit_mutation(json, Verb::Add, &applied, "added", false),
        Err(err) => fail(json, &err),
    }
}

fn list(store: &Client, json: bool, repo: Option<&str>, archived: bool) -> ExitCode {
    match store.list(ListFilter {
        repo: repo.map(str::to_owned),
        archived,
    }) {
        Ok(tasks) if json => crate::output::json(&ListDocument::new(&tasks)),
        Ok(tasks) if tasks.is_empty() => {
            outln!("No tasks. Add one with phux mission add \"<title>\".");
            ExitCode::SUCCESS
        }
        Ok(tasks) => {
            let rows: Vec<(String, &'static str, String)> = tasks
                .iter()
                .map(|task| {
                    (
                        task.alias.display(),
                        task.state.as_words(),
                        task.title.clone(),
                    )
                })
                .collect();
            let borrowed: Vec<(&str, &str, &str)> = rows
                .iter()
                .map(|row| (row.0.as_str(), row.1, row.2.as_str()))
                .collect();
            out!("{}", format_list(&borrowed));
            ExitCode::SUCCESS
        }
        Err(err) => fail(json, &err),
    }
}

fn show(store: &Client, json: bool, alias: &str) -> ExitCode {
    let alias = match parse_alias(json, alias) {
        Ok(alias) => alias,
        Err(code) => return code,
    };
    match store.show(alias) {
        Ok(task) if json => crate::output::json(&ShowDocument::new(&task)),
        Ok(task) => {
            out!(
                "{}",
                format_show(
                    &task.alias.display(),
                    &task.title,
                    task.state.as_words(),
                    task.archived,
                    &task.description,
                    task.revision,
                )
            );
            ExitCode::SUCCESS
        }
        Err(err) => fail(json, &err),
    }
}

fn edit(
    store: &Client,
    json: bool,
    alias: &str,
    title: Option<&str>,
    description: Option<&str>,
    rev: &RevOpt,
    op: &OpOpt,
) -> ExitCode {
    if title.is_none() && description.is_none() {
        return fail(json, &phux_work::Error::EmptyEdit);
    }
    let alias = match parse_alias(json, alias) {
        Ok(alias) => alias,
        Err(code) => return code,
    };
    let expected_revision = match require_expected(store, json, &alias, rev.expected_revision) {
        Ok(revision) => revision,
        Err(code) => return code,
    };
    let operation_id = match operation_id(json, op) {
        Ok(id) => id,
        Err(code) => return code,
    };
    match store.edit(EditObjective {
        alias,
        title: title.map(str::to_owned),
        description: description.map(str::to_owned),
        expected_revision,
        operation_id,
        actor: actor(),
    }) {
        Ok(applied) => emit_mutation(json, Verb::Edit, &applied, "updated", true),
        Err(err) => fail(json, &err),
    }
}

fn set_archived(
    store: &Client,
    json: bool,
    alias: &str,
    rev: &RevOpt,
    op: &OpOpt,
    archive: bool,
) -> ExitCode {
    let alias = match parse_alias(json, alias) {
        Ok(alias) => alias,
        Err(code) => return code,
    };
    let expected_revision = match require_expected(store, json, &alias, rev.expected_revision) {
        Ok(revision) => revision,
        Err(code) => return code,
    };
    let operation_id = match operation_id(json, op) {
        Ok(id) => id,
        Err(code) => return code,
    };
    let request = RevisionEdit {
        alias,
        expected_revision,
        operation_id,
        actor: actor(),
    };
    let result = if archive {
        store.archive(request)
    } else {
        store.restore(request)
    };
    match result {
        Ok(applied) if archive => emit_mutation(json, Verb::Archive, &applied, "archived", true),
        Ok(applied) => emit_mutation(json, Verb::Restore, &applied, "restored", true),
        Err(err) => fail(json, &err),
    }
}

fn emit_mutation(
    json: bool,
    verb: Verb,
    applied: &phux_work::Applied,
    human: &str,
    with_rev: bool,
) -> ExitCode {
    if json {
        crate::output::json(&MutationDocument::from_applied(verb, applied))
    } else if with_rev {
        outln!(
            "{human} {} (rev {})",
            applied.objective.alias.display(),
            applied.objective.revision
        );
        ExitCode::SUCCESS
    } else {
        outln!("{human} {}", applied.objective.alias.display());
        ExitCode::SUCCESS
    }
}

fn require_expected(
    store: &Client,
    json: bool,
    alias: &Alias,
    expected: Option<u64>,
) -> Result<u64, ExitCode> {
    if let Some(expected) = expected {
        return Ok(expected);
    }
    match store.show(alias.clone()) {
        Ok(task) => Err(usage(
            json,
            &format!(
                "{} is at rev {}; rerun with --expected-revision {}",
                task.alias.display(),
                task.revision,
                task.revision
            ),
            "pass --expected-revision",
        )),
        Err(err) => Err(fail(json, &err)),
    }
}

fn format_list(rows: &[(&str, &str, &str)]) -> String {
    let alias_width = rows
        .iter()
        .map(|row| row.0.chars().count())
        .max()
        .unwrap_or(0);
    let state_width = rows
        .iter()
        .map(|row| row.1.chars().count())
        .max()
        .unwrap_or(0);
    let mut text = String::new();
    for (alias, state, title) in rows {
        let _ = writeln!(
            text,
            "{alias:<alias_width$}  {state:<state_width$}  {title}"
        );
    }
    text
}

fn format_show(
    alias: &str,
    title: &str,
    state: &str,
    archived: bool,
    description: &str,
    revision: u64,
) -> String {
    let archived = if archived { "yes" } else { "no" };
    let mut text = format!("{alias}  {title}\nstate: {state}\narchived: {archived}\n");
    if !description.is_empty() {
        let _ = writeln!(text, "description: {description}");
    }
    let _ = writeln!(text, "rev {revision}");
    text
}

pub(crate) fn operation_id(json: bool, op: &OpOpt) -> Result<OperationId, ExitCode> {
    op.operation_id
        .as_deref()
        .map_or_else(OperationId::generate, OperationId::parse)
        .map_err(|err| fail(json, &err))
}

fn parse_alias(json: bool, text: &str) -> Result<Alias, ExitCode> {
    Alias::parse(text).map_err(|err| fail(json, &err))
}

fn default_repo(json: bool) -> Result<String, ExitCode> {
    let cwd = std::env::current_dir().map_err(|err| {
        usage(
            json,
            &format!("could not read the current directory: {err}"),
            "pass --repo",
        )
    })?;
    if let Some(repo) = origin_repo(&cwd) {
        return Ok(repo);
    }
    let Some(name) = cwd.file_name().and_then(|name| name.to_str()) else {
        return Err(usage(
            json,
            "the current directory has no name to use as a repo id",
            "pass --repo",
        ));
    };
    Ok(name.to_owned())
}

/// The ADR-0168 project id when `dir` has an origin remote. A checkout with
/// no network remote, and a directory outside git, yield `None` so the caller
/// can use the directory name.
fn origin_repo(dir: &std::path::Path) -> Option<String> {
    let project = phux_client::project_identity::discover(dir, &super::ls::local_hostname(), None)?;
    if project.id.starts_with("path:") {
        return None;
    }
    Some(project.id)
}

pub(crate) fn actor() -> String {
    std::env::var("USER")
        .ok()
        .filter(|user| !user.is_empty() && user.len() <= 128 && !user.contains('\0'))
        .unwrap_or_else(|| "cli".to_owned())
}

pub(crate) fn report(json: bool, err: &phux_work::Error) -> ExitCode {
    fail(json, err)
}

pub(crate) fn report_code(
    json: bool,
    code: &str,
    message: &str,
    remedy: &str,
    exit: u8,
) -> ExitCode {
    if json {
        emit_json_error(code, message, remedy, exit)
    } else {
        error!("{message}");
        hint!("{remedy}");
        ExitCode::from(exit)
    }
}

fn fail(json: bool, err: &phux_work::Error) -> ExitCode {
    let (code, exit, remedy) = classify(err);
    if json {
        emit_json_error(code, &err.to_string(), remedy, exit)
    } else {
        error!("{}", err);
        hint!("{remedy}");
        ExitCode::from(exit)
    }
}

fn usage(json: bool, message: &str, remedy: &str) -> ExitCode {
    if json {
        emit_json_error(codes::USAGE_ERROR, message, remedy, 2)
    } else {
        error!("{message}");
        hint!("{remedy}");
        ExitCode::from(2)
    }
}

const fn classify(err: &phux_work::Error) -> (&'static str, u8, &'static str) {
    match err {
        phux_work::Error::NotFound { .. } => (
            codes::MISSION_NOT_FOUND,
            1,
            "check the alias with `phux mission list`",
        ),
        phux_work::Error::RevisionConflict { .. } => (
            codes::MISSION_REVISION_CONFLICT,
            1,
            "rerun with --expected-revision set to the revision named in the error",
        ),
        phux_work::Error::OperationConflict { .. } => (
            codes::MISSION_OPERATION_CONFLICT,
            1,
            "use a new operation id; this id already names a different write",
        ),
        phux_work::Error::UnsupportedSchema { .. } => (
            codes::MISSION_UNSUPPORTED_SCHEMA,
            1,
            "this binary reads mission schema version 1 only",
        ),
        phux_work::Error::Busy => (
            codes::MISSION_BUSY,
            1,
            "retry; if it persists, check `keeper.log` beside the work store",
        ),
        phux_work::Error::NotStartable { .. } => (
            codes::MISSION_NOT_STARTABLE,
            1,
            "restore the task before starting it",
        ),
        phux_work::Error::AlreadyStarted { .. } => (
            codes::MISSION_ALREADY_STARTED,
            1,
            "the task already has a live run",
        ),
        phux_work::Error::WorktreeBound { .. } => (
            codes::MISSION_WORKTREE_BOUND,
            1,
            "leave that worktree to the task that holds it",
        ),
        phux_work::Error::AtCapacity => (
            codes::MISSION_START_FAILED,
            1,
            "wait for a running agent to finish",
        ),
        phux_work::Error::Keeper(_) => (
            codes::MISSION_STORE,
            1,
            "retry; if it persists, check `keeper.log` beside the work store",
        ),
        phux_work::Error::InvalidAlias(_)
        | phux_work::Error::InvalidOperationId(_)
        | phux_work::Error::InvalidRepo(_)
        | phux_work::Error::InvalidText { .. }
        | phux_work::Error::EmptyEdit => {
            (codes::USAGE_ERROR, 2, "run `phux mission --help` for usage")
        }
        phux_work::Error::Io(_)
        | phux_work::Error::Sqlite(_)
        | phux_work::Error::Json(_)
        | phux_work::Error::Random(_)
        | phux_work::Error::Corrupt(_)
        | phux_work::Error::Store(_) => (
            codes::MISSION_STORE,
            1,
            "check permissions on the work store",
        ),
    }
}

fn emit_json_error(code: &str, message: &str, remedy: &str, exit: u8) -> ExitCode {
    let doc = serde_json::json!({
        "schema": SCHEMA,
        "schema_version": SCHEMA_VERSION,
        "error": {"code": code, "message": message},
        "remedy": remedy,
        "exit_code": exit,
    });
    match serde_json::to_string(&doc) {
        Ok(line) => eprintln!("{line}"),
        Err(err) => eprintln!("mission error: {message} ({err})"),
    }
    ExitCode::from(exit)
}

#[cfg(test)]
mod tests {
    use super::{format_list, format_show};

    #[test]
    fn list_pads_alias_and_state_and_spells_needs_you() {
        let text = format_list(&[
            ("phux/1", "todo", "Fix"),
            ("github.com/no-phux/phux/2", "needs you", "Grant"),
        ]);
        assert!(!text.contains("needs_you"), "{text}");
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(lines[0].find("todo"), lines[1].find("needs you"), "{text}");
        assert_eq!(lines[0].find("Fix"), lines[1].find("Grant"), "{text}");
        assert!(lines[1].contains("needs you"), "{text}");
    }

    #[test]
    fn show_ends_with_rev_and_spells_the_state() {
        let text = format_show("phux/1", "Fix", "needs you", false, "more", 5);
        assert_eq!(
            text,
            "phux/1  Fix\nstate: needs you\narchived: no\ndescription: more\nrev 5\n"
        );
        assert!(!text.contains("revision:"));
        let bare = format_show("phux/1", "Fix", "todo", true, "", 2);
        assert_eq!(bare, "phux/1  Fix\nstate: todo\narchived: yes\nrev 2\n");
    }
}
