//! `phux project` — named checkouts and trusted session recipes (ADR-0152).
//!
//! A composition layer like `phux worktree` (ADR-0054): it resolves a
//! checkout, names its session with the same pure function, compiles the
//! checkout's `.phux/project.toml` into a workspace archive, and replays it
//! through `workspace restore`'s engine. The server learns nothing about
//! projects. A repository recipe runs only after its exact bytes are
//! approved on this machine; reopening a live session never reruns it.

use std::io::{self, IsTerminal as _, Read as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_server::runtime::default_socket_path;

use crate::commands::json_err::{self, CliError, codes};
use crate::commands::workspace::archive::{RestoreError, restore_archive};
use crate::commands::worktree::{live_session_names, seed_terminal_of, session_name_for};
use crate::commands::{ProjectAction, cli_runtime};

mod recipe;
mod trust;

/// Where a checkout keeps its recipe.
const RECIPE_PATH: &str = ".phux/project.toml";
const SCHEMA_VERSION: u8 = 1;

/// Exit code of `project status` when trust cannot be determined.
const STATUS_UNKNOWN: u8 = 2;

pub(crate) fn run_project(action: &ProjectAction, socket: Option<PathBuf>) -> ExitCode {
    match action {
        ProjectAction::Open { target, json } => run_open(target.as_deref(), *json, socket),
        ProjectAction::List { json } => run_list(*json, socket.as_deref()),
        ProjectAction::Init { target } => run_init(target.as_deref()),
        ProjectAction::Trust { target } => run_trust(target.as_deref()),
        ProjectAction::Untrust { target } => run_untrust(target.as_deref()),
        ProjectAction::Status { target, json } => run_status(target.as_deref(), *json),
    }
}

/// A resolved checkout and the recipe that belongs to it.
#[derive(Debug)]
struct Project {
    /// The catalog name, when `TARGET` named one.
    name: Option<String>,
    /// The checkout root: the git work tree, or the directory itself.
    root: PathBuf,
    /// The repository identity approvals are keyed on.
    identity: PathBuf,
    /// The recipe file (which may not exist).
    recipe: PathBuf,
    /// The recipe came from `config.toml`, so it needs no approval.
    explicit: bool,
}

impl Project {
    fn session(&self) -> String {
        session_name_for(&self.root)
    }

    fn trust_key(&self) -> trust::TrustKey {
        let recipe = self
            .recipe
            .strip_prefix(&self.root)
            .map_or_else(|_| self.recipe.clone(), Path::to_path_buf);
        trust::TrustKey {
            identity: self.identity.clone(),
            recipe,
        }
    }
}

/// Resolve `TARGET`: a `[[projects]]` name first, else a path (default `.`).
fn resolve(target: Option<&str>) -> Result<Project, String> {
    if let Some(name) = target
        && let Some(entry) = catalog()?.into_iter().find(|entry| entry.name == name)
    {
        let path = expand_home(&entry.path)?;
        let mut project = resolve_path(&path)?;
        project.name = Some(entry.name);
        if let Some(recipe) = entry.recipe {
            project.recipe = expand_home(&recipe)?;
            project.explicit = true;
        }
        return Ok(project);
    }
    resolve_path(Path::new(target.unwrap_or(".")))
}

fn resolve_path(path: &Path) -> Result<Project, String> {
    let dir = path
        .canonicalize()
        .map_err(|err| format!("{}: {err}", path.display()))?;
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    let (root, identity) = match git_root(&dir) {
        Some((root, common)) => (root, common),
        None => (dir.clone(), dir),
    };
    let recipe = root.join(RECIPE_PATH);
    Ok(Project {
        name: None,
        root,
        identity,
        recipe,
        explicit: false,
    })
}

/// `(work tree root, canonical common dir)` for a directory inside git; the
/// common dir is what every worktree of one repository shares.
fn git_root(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    use crate::commands::workspace::git_text;
    let root = PathBuf::from(
        git_text(dir, &["rev-parse", "--show-toplevel"])
            .ok()?
            .trim(),
    )
    .canonicalize()
    .ok()?;
    let common = git_text(&root, &["rev-parse", "--git-common-dir"]).ok()?;
    let common = root.join(common.trim()).canonicalize().ok()?;
    Some((root, common))
}

fn catalog() -> Result<Vec<phux_config::ProjectConfigEntry>, String> {
    let config =
        phux_config::loader::load().map_err(|err| format!("could not read config: {err}"))?;
    phux_config::toml_registry::reject_duplicate_names(
        config.projects.iter().map(|entry| entry.name.as_str()),
        "project",
    )?;
    Ok(config.projects)
}

/// `~/x` against `$HOME`; anything else must already be absolute.
fn expand_home(path: &Path) -> Result<PathBuf, String> {
    if let Ok(rest) = path.strip_prefix("~") {
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .ok_or_else(|| format!("{}: HOME is not set", path.display()))?;
        return Ok(PathBuf::from(home).join(rest));
    }
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Err(format!(
        "{}: catalog paths must be absolute or start with `~/`",
        path.display()
    ))
}

/// The recipe's bytes, `None` when the file does not exist.
fn read_recipe(path: &Path) -> Result<Option<String>, String> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("{}: {err}", path.display())),
    };
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if meta.len() > recipe::MAX_RECIPE_BYTES {
        return Err(format!(
            "{} is larger than {} bytes",
            path.display(),
            recipe::MAX_RECIPE_BYTES
        ));
    }
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|mut file| file.read_to_string(&mut text))
        .map_err(|err| format!("{}: {err}", path.display()))?;
    Ok(Some(text))
}

/// A validated recipe ready to compile, with its digest.
struct Loaded {
    text: String,
    sha256: String,
    recipe: recipe::Recipe,
}

/// Read and fully validate the project's recipe, `None` when there is none.
fn load(project: &Project) -> Result<Option<Loaded>, String> {
    let Some(text) = read_recipe(&project.recipe)? else {
        if project.explicit {
            return Err(format!(
                "recipe {} from config.toml does not exist",
                project.recipe.display()
            ));
        }
        return Ok(None);
    };
    let parsed =
        recipe::parse(&text).map_err(|err| format!("{}: {err}", project.recipe.display()))?;
    // Compile once now so every check runs before trust is asked or recorded.
    recipe::compile(Some(&parsed), &project.root, &project.session())
        .map_err(|err| format!("{}: {err}", project.recipe.display()))?;
    Ok(Some(Loaded {
        sha256: trust::digest(text.as_bytes()),
        text,
        recipe: parsed,
    }))
}

// ---------------------------------------------------------------------------
// open
// ---------------------------------------------------------------------------

fn run_open(target: Option<&str>, json: bool, socket: Option<PathBuf>) -> ExitCode {
    if !json && let Err(code) = super::attach::interactive_tty_preflight() {
        return code;
    }
    let project = match resolve(target) {
        Ok(project) => project,
        Err(err) => {
            return refuse(
                json,
                codes::WORKSPACE,
                &err,
                "pass a directory or a `[[projects]]` name",
            );
        }
    };
    let session = project.session();
    let socket_path = socket.clone().unwrap_or_else(default_socket_path);
    if let Err(code) = super::ensure_socket_path_fits(&socket_path) {
        return code;
    }
    if !json && let Err(code) = super::attach::nested_attach_guard(&socket_path) {
        return code;
    }

    // A live session is reopened as it is; its recipe is never rerun.
    let live = live_session_names(Some(&socket_path)).is_some_and(|names| names.contains(&session));
    let created = if live {
        false
    } else {
        match create(&project, &session, &socket_path, json) {
            Ok(()) => true,
            Err(code) => return code,
        }
    };

    if !json {
        return super::attach::run_attach(Some(session), socket);
    }
    let Some(terminal_id) = seed_terminal_of(&session, Some(&socket_path)) else {
        return refuse(
            true,
            codes::NO_SUCH_TARGET,
            &format!("session '{session}' reported no local pane to address"),
            "run `phux ls --json` to see what the server holds for that session",
        );
    };
    crate::output::json(&serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "project": project.name,
        "path": project.root,
        "session": session,
        "created": created,
        "terminal_id": terminal_id,
    }))
}

/// Build the session from the recipe (asking for trust when needed).
fn create(
    project: &Project,
    session: &str,
    socket_path: &Path,
    json: bool,
) -> Result<(), ExitCode> {
    let loaded = load(project).map_err(|err| {
        refuse(
            json,
            codes::WORKSPACE,
            &err,
            "fix the recipe, then open the project again",
        )
    })?;
    if let Some(loaded) = &loaded {
        approve_for_open(project, loaded, json)?;
    }
    let archive = recipe::compile(
        loaded.as_ref().map(|loaded| &loaded.recipe),
        &project.root,
        session,
    )
    .map_err(|err| {
        refuse(
            json,
            codes::WORKSPACE,
            &err,
            "fix the recipe, then open the project again",
        )
    })?;

    if let Err(err) = super::server::ensure_server_unseeded(socket_path, json) {
        tracing::debug!(error = %err, "auto-spawn failed on the project open path");
    }
    let rt = cli_runtime()?;
    let summary = match rt.block_on(restore_archive(socket_path, &archive, "project open")) {
        Ok(summary) => summary,
        Err(RestoreError::Reported(code)) => return Err(code),
        Err(RestoreError::Failed(err)) => {
            return Err(refuse(
                json,
                codes::WORKSPACE,
                &err,
                "fix the recipe, then open the project again",
            ));
        }
    };
    for warning in &summary.warnings {
        eprintln!("phux: warning: {}", warning.message);
    }
    if let Some(failed) = summary.failed.first() {
        return Err(refuse(
            json,
            codes::WORKSPACE,
            &format!(
                "could not build session '{}': {}",
                failed.name, failed.reason
            ),
            "every pane it started was closed again; fix the cause and retry",
        ));
    }
    Ok(())
}

/// Admit `loaded` for this open: config-owned, already approved, or approved
/// now at the terminal after the user reads it.
fn approve_for_open(project: &Project, loaded: &Loaded, json: bool) -> Result<(), ExitCode> {
    if project.explicit {
        return Ok(());
    }
    let key = project.trust_key();
    let dir = trust::default_dir();
    match trust::is_trusted(&dir, &key, &loaded.sha256) {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(err) => {
            return Err(refuse(
                json,
                codes::WORKSPACE,
                &err,
                "repair or remove the trust store",
            ));
        }
    }
    let remedy = format!(
        "read {} and run `phux project trust {}` to approve it",
        project.recipe.display(),
        project.root.display()
    );
    if json || !io::stdin().is_terminal() {
        return Err(refuse_with(
            json,
            codes::WORKSPACE,
            &format!("project recipe {} is not trusted", project.recipe.display()),
            &remedy,
            super::confirm::NOT_CONFIRMED,
        ));
    }
    eprintln!(
        "phux: {} can run commands and has not been approved on this machine:\n",
        project.recipe.display()
    );
    for line in loaded.text.lines() {
        eprintln!("    {line}");
    }
    eprintln!();
    super::confirm::consent(false, true, "trust and run this recipe", || {
        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .ok()
            .filter(|read| *read > 0)
            .map(|_| line)
    })?;
    trust::trust(&dir, &key, &loaded.sha256).map_err(|err| {
        refuse(
            false,
            codes::WORKSPACE,
            &err,
            "repair or remove the trust store",
        )
    })
}

// ---------------------------------------------------------------------------
// list / init / trust / untrust / status
// ---------------------------------------------------------------------------

fn run_list(json: bool, socket: Option<&Path>) -> ExitCode {
    let entries = match catalog() {
        Ok(entries) => entries,
        Err(err) => return refuse(json, codes::WORKSPACE, &err, "run `phux config check`"),
    };
    let live = live_session_names(socket);
    let rows: Vec<_> = entries
        .iter()
        .map(|entry| {
            let resolved = expand_home(&entry.path)
                .ok()
                .and_then(|path| resolve_path(&path).ok());
            let session = resolved.as_ref().map(Project::session);
            let open = session
                .as_ref()
                .and_then(|session| live.as_ref().map(|names| names.contains(session)));
            (entry, resolved, session, open)
        })
        .collect();
    if json {
        let rows: Vec<_> = rows
            .iter()
            .map(|(entry, resolved, session, open)| {
                serde_json::json!({
                    "name": entry.name,
                    "path": entry.path,
                    "root": resolved.as_ref().map(|project| &project.root),
                    "recipe": entry.recipe,
                    "session": session,
                    "open": open,
                })
            })
            .collect();
        return crate::output::json(&serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "projects": rows,
        }));
    }
    if rows.is_empty() {
        outln!("No projects. Add `[[projects]]` entries to config.toml (see `phux config path`).");
        return ExitCode::SUCCESS;
    }
    for (entry, resolved, _, open) in &rows {
        let state = match (resolved, open) {
            (None, _) => "missing",
            (Some(_), Some(true)) => "open",
            (Some(_), Some(false)) => "-",
            (Some(_), None) => "?",
        };
        outln!("{:<20} {:<8} {}", entry.name, state, entry.path.display());
    }
    ExitCode::SUCCESS
}

fn run_init(target: Option<&str>) -> ExitCode {
    let project = match resolve(target) {
        Ok(project) => project,
        Err(err) => return fail(&err),
    };
    if project.explicit {
        return fail(&format!(
            "project '{}' uses the recipe {} from config.toml; edit that file instead",
            project.name.as_deref().unwrap_or_default(),
            project.recipe.display()
        ));
    }
    if project.recipe.exists() {
        return fail(&format!("{} already exists", project.recipe.display()));
    }
    if let Some(parent) = project.recipe.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        return fail(&format!("could not create {}: {err}", parent.display()));
    }
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&project.recipe)
        .and_then(|mut file| io::Write::write_all(&mut file, recipe::STARTER.as_bytes()));
    if let Err(err) = written {
        return fail(&format!(
            "could not write {}: {err}",
            project.recipe.display()
        ));
    }
    // The starter is phux's own text, so approving it trusts nothing new;
    // the first edit makes the file untrusted again.
    let sha256 = trust::digest(recipe::STARTER.as_bytes());
    if let Err(err) = trust::trust(&trust::default_dir(), &project.trust_key(), &sha256) {
        eprintln!("phux: warning: could not approve the starter recipe: {err}");
    }
    outln!(
        "Wrote {} (approved as written). After editing it, run `phux project trust`; \
         `phux project open` builds the session.",
        project.recipe.display()
    );
    ExitCode::SUCCESS
}

fn run_trust(target: Option<&str>) -> ExitCode {
    let project = match resolve(target) {
        Ok(project) => project,
        Err(err) => return fail(&err),
    };
    if project.explicit {
        outln!(
            "{} comes from config.toml and is already trusted.",
            project.recipe.display()
        );
        return ExitCode::SUCCESS;
    }
    let loaded = match load(&project) {
        Ok(Some(loaded)) => loaded,
        Ok(None) => return fail(&format!("{} does not exist", project.recipe.display())),
        Err(err) => return fail(&err),
    };
    if let Err(err) = trust::trust(&trust::default_dir(), &project.trust_key(), &loaded.sha256) {
        return fail(&err);
    }
    outln!(
        "Trusted {} (sha256 {}).",
        project.recipe.display(),
        loaded.sha256
    );
    ExitCode::SUCCESS
}

fn run_untrust(target: Option<&str>) -> ExitCode {
    let project = match resolve(target) {
        Ok(project) => project,
        Err(err) => return fail(&err),
    };
    if project.explicit {
        return fail(&format!(
            "{} is trusted by config.toml; remove its `recipe` key to revoke it",
            project.recipe.display()
        ));
    }
    match trust::untrust(&trust::default_dir(), &project.trust_key()) {
        Ok(true) => outln!("Revoked approval of {}.", project.recipe.display()),
        Ok(false) => outln!("{} was not approved.", project.recipe.display()),
        Err(err) => return fail(&err),
    }
    ExitCode::SUCCESS
}

/// Exit 0 trusted, 1 untrusted or absent, 2 undeterminable — for `if`.
fn run_status(target: Option<&str>, json: bool) -> ExitCode {
    let project = match resolve(target) {
        Ok(project) => project,
        Err(err) => return status_unknown(json, &err),
    };
    let text = match read_recipe(&project.recipe) {
        Ok(text) => text,
        Err(err) => return status_unknown(json, &err),
    };
    let sha256 = text.as_deref().map(|text| trust::digest(text.as_bytes()));
    let trusted = match (&sha256, project.explicit) {
        (None, _) => false,
        (Some(_), true) => true,
        (Some(sha256), false) => {
            match trust::is_trusted(&trust::default_dir(), &project.trust_key(), sha256) {
                Ok(trusted) => trusted,
                Err(err) => return status_unknown(json, &err),
            }
        }
    };
    if json {
        let _ = crate::output::json(&serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "path": project.root,
            "recipe": project.recipe,
            "present": sha256.is_some(),
            "explicit": project.explicit,
            "trusted": trusted,
            "sha256": sha256,
        }));
    } else {
        let state = match (&sha256, trusted) {
            (None, _) => "absent",
            (Some(_), true) => "trusted",
            (Some(_), false) => "untrusted",
        };
        outln!("{state} {}", project.recipe.display());
    }
    if trusted {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn status_unknown(json: bool, err: &str) -> ExitCode {
    refuse_with(
        json,
        codes::WORKSPACE,
        err,
        "check the path and the trust store",
        STATUS_UNKNOWN,
    )
}

fn refuse(json: bool, code: &'static str, message: &str, remedy: &str) -> ExitCode {
    refuse_with(json, code, message, remedy, 1)
}

/// One failure, as the shared `--json` error document or a prose line.
fn refuse_with(json: bool, code: &'static str, message: &str, remedy: &str, exit: u8) -> ExitCode {
    if json {
        return json_err::emit(true, &CliError::new(code, message.to_owned(), remedy), exit);
    }
    eprintln!("phux: {message}\n      {remedy}");
    ExitCode::from(exit)
}

fn fail(message: &str) -> ExitCode {
    eprintln!("phux: {message}");
    ExitCode::FAILURE
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn worktrees_of_one_repository_share_an_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).expect("mkdir");
        let git = |args: &[&str], cwd: &Path| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .status()
                .expect("git");
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q"], &repo);
        git(&["commit", "-q", "--allow-empty", "-m", "x"], &repo);
        git(&["worktree", "add", "-q", "../wt"], &repo);
        std::fs::create_dir(repo.join("sub")).expect("mkdir");

        let main = resolve_path(&repo.join("sub")).expect("main");
        let linked = resolve_path(&dir.path().join("wt")).expect("linked");
        assert_eq!(main.root, repo.canonicalize().expect("canonical"));
        assert_eq!(main.identity, linked.identity);
        assert_ne!(main.root, linked.root);
        assert_eq!(main.trust_key(), linked.trust_key());
        assert_eq!(main.trust_key().recipe, Path::new(RECIPE_PATH));
        assert_eq!(linked.session(), "wt");

        let plain = resolve_path(dir.path()).expect("plain dir");
        assert_eq!(plain.identity, plain.root);
    }

    #[test]
    fn catalog_paths_expand_home_and_refuse_relative() {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        if let Some(home) = home.filter(|home| !home.as_os_str().is_empty()) {
            assert_eq!(
                expand_home(Path::new("~/code")).expect("tilde"),
                home.join("code")
            );
        }
        assert_eq!(
            expand_home(Path::new("/abs")).expect("abs"),
            PathBuf::from("/abs")
        );
        assert!(expand_home(Path::new("rel/path")).is_err());
    }
}
