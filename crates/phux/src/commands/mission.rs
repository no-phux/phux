//! `phux mission`: the local task store. No server is contacted.

use std::process::ExitCode;

use phux_mission::{
    AddObjective, Alias, DescribeObjective, ListDocument, ListFilter, MutationDocument,
    OperationId, RevisionEdit, SCHEMA, SCHEMA_VERSION, ShowDocument, Verb,
};

use super::json_err::codes;

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
        /// Defaults to the current directory's name.
        #[usage(long, value_name = "ID")]
        repo: Option<String>,
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
    /// Set a task's title or description
    Describe {
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
}

#[derive(Debug, usage::Args)]
pub(crate) struct OpOpt {
    /// Repeat a write safely: 32 hex digits. The same id returns the first result.
    #[usage(long, value_name = "HEX32")]
    operation_id: Option<String>,
}

#[derive(Debug, usage::Args)]
pub(crate) struct RevOpt {
    /// Refuse the write unless the task revision is N.
    #[usage(long, value_name = "N")]
    expected_revision: Option<u64>,
}

pub(crate) fn run(action: Option<MissionAction>) -> ExitCode {
    action.map_or_else(print_help, run_action)
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

fn run_action(action: MissionAction) -> ExitCode {
    let json = wants_json(&action);
    let mut store = match open_store(json) {
        Ok(store) => store,
        Err(code) => return code,
    };
    match action {
        MissionAction::Add {
            title,
            description,
            repo,
            op,
            ..
        } => add(
            &mut store,
            json,
            &title,
            description.as_deref(),
            repo.as_deref(),
            &op,
        ),
        MissionAction::List { repo, archived, .. } => list(&store, json, repo.as_deref(), archived),
        MissionAction::Show { alias, .. } => show(&store, json, &alias),
        MissionAction::Describe {
            alias,
            title,
            description,
            rev,
            op,
            ..
        } => describe(
            &mut store,
            json,
            &alias,
            title.as_deref(),
            description.as_deref(),
            &rev,
            &op,
        ),
        MissionAction::Archive { alias, rev, op, .. } => {
            archive(&mut store, json, &alias, &rev, &op)
        }
        MissionAction::Restore { alias, rev, op, .. } => {
            restore(&mut store, json, &alias, &rev, &op)
        }
    }
}

const fn wants_json(action: &MissionAction) -> bool {
    match action {
        MissionAction::Add { json, .. }
        | MissionAction::List { json, .. }
        | MissionAction::Show { json, .. }
        | MissionAction::Describe { json, .. }
        | MissionAction::Archive { json, .. }
        | MissionAction::Restore { json, .. } => json.json,
    }
}

fn open_store(json: bool) -> Result<phux_mission::Store, ExitCode> {
    let path = phux_mission::store_path(&phux_config::instance::state_dir());
    phux_mission::Store::open(&path).map_err(|err| fail(json, &err))
}

fn add(
    store: &mut phux_mission::Store,
    json: bool,
    title: &str,
    description: Option<&str>,
    repo: Option<&str>,
    op: &OpOpt,
) -> ExitCode {
    let repo = match repo
        .map(str::to_owned)
        .map_or_else(|| default_repo(json), Ok)
    {
        Ok(repo) => repo,
        Err(code) => return code,
    };
    let operation_id = match operation_id(json, op) {
        Ok(id) => id,
        Err(code) => return code,
    };
    match store.add(AddObjective {
        repo_project_id: repo,
        title: title.to_owned(),
        description: description.unwrap_or("").to_owned(),
        operation_id,
        actor: actor(),
    }) {
        Ok(applied) => emit_mutation(json, Verb::Add, &applied, "added"),
        Err(err) => fail(json, &err),
    }
}

fn list(store: &phux_mission::Store, json: bool, repo: Option<&str>, archived: bool) -> ExitCode {
    match store.list(&ListFilter {
        repo: repo.map(str::to_owned),
        archived,
    }) {
        Ok(tasks) if json => crate::output::json(&ListDocument::new(&tasks)),
        Ok(tasks) if tasks.is_empty() => {
            outln!("no tasks");
            ExitCode::SUCCESS
        }
        Ok(tasks) => {
            for task in tasks {
                outln!(
                    "{}  {}  {}",
                    task.alias.display(),
                    task.state.as_str(),
                    task.title
                );
            }
            ExitCode::SUCCESS
        }
        Err(err) => fail(json, &err),
    }
}

fn show(store: &phux_mission::Store, json: bool, alias: &str) -> ExitCode {
    let alias = match parse_alias(json, alias) {
        Ok(alias) => alias,
        Err(code) => return code,
    };
    match store.show(&alias) {
        Ok(task) if json => crate::output::json(&ShowDocument::new(&task)),
        Ok(task) => {
            outln!("{}  {}", task.alias.display(), task.title);
            outln!("state: {}", task.state.as_str());
            outln!("revision: {}", task.revision);
            outln!("archived: {}", if task.archived { "yes" } else { "no" });
            if !task.description.is_empty() {
                outln!("description: {}", task.description);
            }
            ExitCode::SUCCESS
        }
        Err(err) => fail(json, &err),
    }
}

fn describe(
    store: &mut phux_mission::Store,
    json: bool,
    alias: &str,
    title: Option<&str>,
    description: Option<&str>,
    rev: &RevOpt,
    op: &OpOpt,
) -> ExitCode {
    if title.is_none() && description.is_none() {
        return usage(
            json,
            "describe needs --title or --description",
            "pass the field to change",
        );
    }
    let alias = match parse_alias(json, alias) {
        Ok(alias) => alias,
        Err(code) => return code,
    };
    let operation_id = match operation_id(json, op) {
        Ok(id) => id,
        Err(code) => return code,
    };
    match store.describe(DescribeObjective {
        alias,
        title: title.map(str::to_owned),
        description: description.map(str::to_owned),
        expected_revision: rev.expected_revision,
        operation_id,
        actor: actor(),
    }) {
        Ok(applied) => emit_mutation(json, Verb::Describe, &applied, "described"),
        Err(err) => fail(json, &err),
    }
}

fn archive(
    store: &mut phux_mission::Store,
    json: bool,
    alias: &str,
    rev: &RevOpt,
    op: &OpOpt,
) -> ExitCode {
    edit(store, json, alias, rev, op, true)
}

fn restore(
    store: &mut phux_mission::Store,
    json: bool,
    alias: &str,
    rev: &RevOpt,
    op: &OpOpt,
) -> ExitCode {
    edit(store, json, alias, rev, op, false)
}

fn edit(
    store: &mut phux_mission::Store,
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
    let operation_id = match operation_id(json, op) {
        Ok(id) => id,
        Err(code) => return code,
    };
    let request = RevisionEdit {
        alias,
        expected_revision: rev.expected_revision,
        operation_id,
        actor: actor(),
    };
    let result = if archive {
        store.archive(request)
    } else {
        store.restore(request)
    };
    match result {
        Ok(applied) if archive => emit_mutation(json, Verb::Archive, &applied, "archived"),
        Ok(applied) => emit_mutation(json, Verb::Restore, &applied, "restored"),
        Err(err) => fail(json, &err),
    }
}

fn emit_mutation(json: bool, verb: Verb, applied: &phux_mission::Applied, human: &str) -> ExitCode {
    if json {
        crate::output::json(&MutationDocument::from_applied(verb, applied))
    } else {
        outln!("{human} {}", applied.objective.alias.display());
        ExitCode::SUCCESS
    }
}

fn operation_id(json: bool, op: &OpOpt) -> Result<OperationId, ExitCode> {
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
    let Some(name) = cwd.file_name().and_then(|name| name.to_str()) else {
        return Err(usage(
            json,
            "the current directory has no name to use as a repo id",
            "pass --repo",
        ));
    };
    Ok(name.to_owned())
}

fn actor() -> String {
    std::env::var("USER")
        .ok()
        .filter(|user| !user.is_empty() && user.len() <= 128 && !user.contains('\0'))
        .unwrap_or_else(|| "cli".to_owned())
}

fn fail(json: bool, err: &phux_mission::Error) -> ExitCode {
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

const fn classify(err: &phux_mission::Error) -> (&'static str, u8, &'static str) {
    match err {
        phux_mission::Error::NotFound { .. } => (
            codes::MISSION_NOT_FOUND,
            1,
            "check the alias with `phux mission list`",
        ),
        phux_mission::Error::RevisionConflict { .. } => (
            codes::MISSION_REVISION_CONFLICT,
            1,
            "re-read the task and retry with its current revision",
        ),
        phux_mission::Error::OperationConflict { .. } => (
            codes::MISSION_OPERATION_CONFLICT,
            1,
            "use a new operation id; this id already names a different write",
        ),
        phux_mission::Error::UnsupportedSchema { .. } => (
            codes::MISSION_UNSUPPORTED_SCHEMA,
            1,
            "this binary reads mission schema version 1 only",
        ),
        phux_mission::Error::Busy => (
            codes::MISSION_BUSY,
            1,
            "retry after the other writer finishes",
        ),
        phux_mission::Error::InvalidAlias(_)
        | phux_mission::Error::InvalidOperationId(_)
        | phux_mission::Error::InvalidRepo(_)
        | phux_mission::Error::InvalidText { .. }
        | phux_mission::Error::EmptyEdit => {
            (codes::USAGE_ERROR, 2, "run `phux mission --help` for usage")
        }
        phux_mission::Error::Io(_)
        | phux_mission::Error::Sqlite(_)
        | phux_mission::Error::Json(_)
        | phux_mission::Error::Random(_)
        | phux_mission::Error::Corrupt(_) => (
            codes::MISSION_STORE,
            1,
            "check permissions on the mission store",
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
