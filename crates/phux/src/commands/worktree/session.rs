//! Worktree creation and opening, with their persistent session bindings.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_server::runtime::default_socket_path;

use crate::commands::{attach, cli_runtime, ensure_socket_path_fits, new, server, server_target};

use super::{
    binding_json, collect, fail_json, git_bytes, live_session_names, repo_root, report,
    resolve_target, session_name_for,
};

/// The resolved arguments of `phux worktree new`.
pub(super) struct NewRequest<'a> {
    pub(super) branch: &'a str,
    pub(super) path: Option<&'a Path>,
    pub(super) from: Option<&'a str>,
    pub(super) session: Option<&'a str>,
    pub(super) repo: &'a Path,
    pub(super) socket: Option<PathBuf>,
    pub(super) attach: bool,
    pub(super) json: bool,
    pub(super) command: Vec<String>,
}

pub(super) fn run_new(req: NewRequest<'_>) -> ExitCode {
    let NewRequest {
        branch,
        path,
        from,
        session,
        repo,
        socket,
        attach,
        json,
        command,
    } = req;
    let root = match repo_root(repo) {
        Ok(root) => root,
        Err(err) => {
            return fail_json(
                json,
                crate::commands::json_err::codes::WORKSPACE,
                &err,
                "run this inside a git repository, or point --repo at one",
            );
        }
    };

    // Default sibling layout `<repo-parent>/<repo-name>-<branch>`: a nested
    // worktree is one `rm -rf` from the checkout, and upward walks would find the
    // parent's `.git`.
    let dest = path.map_or_else(|| default_worktree_path(&root, branch), Path::to_path_buf);

    if dest.exists() {
        return fail_json(
            json,
            crate::commands::json_err::codes::WORKSPACE,
            &format!(
                "{} already exists — pass --path to choose another location",
                dest.display()
            ),
            "pass --path to put the worktree somewhere else",
        );
    }

    let name = session.map_or_else(|| session_name_for(&dest), ToOwned::to_owned);

    // Refuse a derived-name collision rather than adopting another
    // worktree's session (ADR-0054 §Tradeoffs).
    if session.is_none()
        && let Ok(existing) = collect(&root)
        && let Some(other) = existing
            .iter()
            .find(|entry| session_name_for(&entry.path) == name)
    {
        return fail_json(
            json,
            crate::commands::json_err::codes::WORKSPACE,
            &format!(
                "session name '{name}' is already derived by worktree {} — pass -s NAME",
                other.path.display()
            ),
            "pass -s NAME to give this worktree its own session name",
        );
    }

    // An existing local branch is checked out; a missing one is created.
    // Asking git which case applies is cheaper and more honest than parsing
    // the failure text of a wrong guess.
    let exists = branch_exists(&root, branch);
    let dest_str = dest.to_string_lossy().into_owned();
    let mut args: Vec<&str> = vec!["worktree", "add"];
    if !exists {
        args.push("-b");
        args.push(branch);
    }
    args.push(&dest_str);
    if exists {
        args.push(branch);
    } else if let Some(start) = from {
        args.push(start);
    }

    if let Err(err) = git_bytes(&root, &args) {
        return fail_json(
            json,
            crate::commands::json_err::codes::WORKSPACE,
            &err,
            "git refused the checkout; the message above is git's own",
        );
    }
    // Under `--json` stdout is the document and nothing else, so the prose
    // confirmation is suppressed rather than interleaved into it.
    if !json {
        outln!("worktree {} {branch}", dest.display());
    }
    let socket_path = socket.clone().unwrap_or_else(default_socket_path);
    fire_worktree_created(&dest, branch, &root, &name, &socket_path);

    bind_session(Binding {
        name: &name,
        cwd: &dest,
        branch: Some(branch),
        socket,
        command,
        attach,
        json,
    })
}

/// Run the `worktree-created` hooks (ADR-0168): config `[[hooks.worktree-created]]`
/// and enabled plugins' `[[events]]` on it, here in the CLI because the
/// server never sees a worktree made. Each runs to completion or its
/// timeout before the session starts, so a plugin can prepare the checkout.
/// An unreadable config runs nothing; the session path reports it.
fn fire_worktree_created(worktree: &Path, branch: &str, repo: &Path, session: &str, socket: &Path) {
    let Ok(config) = phux_config::loader::load() else {
        return;
    };
    let catalog =
        phux_server::hooks::HookCatalog::from_config(&config, &phux_config::loader::config_path());
    if catalog.is_empty() {
        return;
    }
    let Ok(rt) = cli_runtime() else {
        return;
    };
    let event = phux_server::hooks::HookEvent::worktree_created(worktree, branch, repo, session);
    rt.block_on(phux_server::hooks::run_to_completion(
        &catalog,
        &event,
        Some(socket),
        Some(phux_plugin::run_log::default_path()),
    ));
}

/// Everything [`bind_session`] needs. Headless by default; `--attach` opts
/// into the interactive path.
struct Binding<'a> {
    name: &'a str,
    cwd: &'a Path,
    /// Echoed into the `--json` document. `None` for a detached checkout,
    /// which `worktree open` can legitimately be pointed at.
    branch: Option<&'a str>,
    socket: Option<PathBuf>,
    command: Vec<String>,
    attach: bool,
    json: bool,
}

fn bind_session(req: Binding<'_>) -> ExitCode {
    let Binding {
        name,
        cwd,
        branch,
        socket,
        command,
        attach,
        json,
    } = req;
    if attach {
        // `--json` and `--attach` are mutually exclusive at the clap level:
        // an attached session owns the terminal, so there is no stdout left
        // to put a document on.
        return new::run_new(
            None,
            Some(name.to_owned()),
            Some(cwd.to_path_buf()),
            server_target::ServerSpec::local(socket),
            new::NewMode::default(),
            command,
            Vec::new(),
        );
    }

    let socket_path = socket.unwrap_or_else(default_socket_path);
    if let Err(code) = ensure_socket_path_fits(&socket_path) {
        return code;
    }
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    // Auto-spawn unseeded (ADR-0105): the session we are about to create is
    // the only one, rather than one beside a stray `default` seed.
    if let Err(err) = server::ensure_server_unseeded(&socket_path, json) {
        // Under `--json` the failure stays off stderr, which carries only the
        // error document; the verb reports it there (as
        // `error.auto_start_error`) when the dial fails.
        if json {
            tracing::debug!(error = %err, "auto-spawn failed on the --json worktree path");
        } else {
            warn!("auto-spawn skipped ({err}). Start a server manually with `phux server`.");
        }
    }

    let command = if command.is_empty() {
        None
    } else {
        Some(command)
    };
    match rt.block_on(new::create_session_via_metadata(
        &server_target::ServerTarget::local(&socket_path),
        name,
        command,
        Some(cwd.to_string_lossy().into_owned()),
        BTreeMap::default(),
        None,
        json,
        None,
        // A worktree's session is named after it: persistent.
        phux_protocol::wire::frame::SessionLifetime::Persistent,
        &[],
    )) {
        // The create already told us the seed pane's id; `--json` just stops
        // throwing it away.
        Ok(terminal_id) => emit_binding(json, branch, cwd, name, terminal_id),
        Err(code) => code,
    }
}

/// A session created for one mission task. The name is derived from the
/// worktree path. Prints nothing on success. The seed pane stays the
/// worktree shell; the worker pane is launched separately.
pub(crate) struct TaskSession {
    /// Session name bound to the worktree.
    pub session: String,
}

/// `git worktree add` plus a persistent session whose attributes include
/// `mission.task`. The caller prints the mission document.
pub(crate) fn create_task_worktree(
    repo: &Path,
    branch: &str,
    dest: &Path,
    from: &str,
    socket: &Path,
    mission_task: &str,
) -> Result<TaskSession, ExitCode> {
    if let Some(parent) = dest.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        error!("could not create {}: {err}", parent.display());
        return Err(ExitCode::FAILURE);
    }
    let dest_str = dest.to_string_lossy().into_owned();
    if let Err(err) = git_bytes(repo, &["worktree", "add", "-b", branch, &dest_str, from]) {
        error!("{err}");
        return Err(ExitCode::FAILURE);
    }
    let name = session_name_for(dest);
    let socket_path = socket.to_path_buf();
    fire_worktree_created(dest, branch, repo, &name, &socket_path);
    let rt = cli_runtime()?;
    if let Err(err) = server::ensure_server_unseeded(&socket_path, false) {
        warn!("auto-spawn skipped ({err}). Start a server manually with `phux server`.");
    }
    let extra = vec![("mission.task".to_owned(), mission_task.to_owned())];
    match rt.block_on(new::create_session_via_metadata(
        &server_target::ServerTarget::local(&socket_path),
        &name,
        None,
        Some(dest.to_string_lossy().into_owned()),
        BTreeMap::new(),
        None,
        false,
        None,
        phux_protocol::wire::frame::SessionLifetime::Persistent,
        &extra,
    )) {
        Ok(_terminal_id) => Ok(TaskSession { session: name }),
        Err(code) => Err(code),
    }
}

/// Report what `new` / `open` just bound: the bare session name for a human,
/// the full document for a machine.
fn emit_binding(
    json: bool,
    branch: Option<&str>,
    path: &Path,
    session: &str,
    terminal_id: u64,
) -> ExitCode {
    if !json {
        outln!(
            "Created session {session} for {}. Next: `phux attach {session}`.",
            path.display()
        );
        return ExitCode::SUCCESS;
    }
    crate::output::json(&binding_json(branch, path, session, terminal_id))
}

pub(super) fn default_worktree_path(root: &Path, branch: &str) -> PathBuf {
    let repo_name = root
        .file_name()
        .map_or_else(|| "repo".to_owned(), |n| n.to_string_lossy().into_owned());
    // The branch may contain `/` (`feat/foo`); flatten it so the worktree is
    // one directory, not a nested tree the operator did not ask for.
    let flat: String = branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let parent = root.parent().unwrap_or(root);
    parent.join(format!("{repo_name}-{flat}"))
}

fn branch_exists(root: &Path, branch: &str) -> bool {
    git_bytes(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
}

pub(super) fn run_open(
    target: &str,
    repo: &Path,
    socket: Option<PathBuf>,
    attach: bool,
    json: bool,
) -> ExitCode {
    let (_root, entry) = match resolve_target(target, repo) {
        Ok(found) => found,
        Err(err) => return report(json, &err),
    };
    let name = session_name_for(&entry.path);

    // Already live: report and exit 0 rather than erroring on the duplicate.
    // `open` is idempotent by design — scripts and keybindings call it
    // without checking first. With `--attach`, join the live session.
    if live_session_names(socket.as_deref()).is_some_and(|names| names.contains(&name)) {
        if attach {
            return attach::run_attach(Some(name), socket);
        }
        if json {
            // A repeated `open` must return the same document, seed pane included.
            let Some(terminal_id) = seed_terminal_of(&name, socket.as_deref()) else {
                return fail_json(
                    true,
                    crate::commands::json_err::codes::NO_SUCH_TARGET,
                    &format!("session '{name}' is live but reported no local pane to address"),
                    "run `phux ls --json` to see what the server holds for that session",
                );
            };
            return emit_binding(
                true,
                entry.branch.as_deref(),
                &entry.path,
                &name,
                u64::from(terminal_id),
            );
        }
        outln!("Session {name} is already open. Next: `phux attach {name}`.");
        return ExitCode::SUCCESS;
    }

    bind_session(Binding {
        name: &name,
        cwd: &entry.path,
        branch: entry.branch.as_deref(),
        socket,
        command: Vec::new(),
        attach,
        json,
    })
}

/// The seed pane of live session `name`: the lowest (oldest) local id, so
/// every `open` answers the same. Satellite panes are skipped.
pub(crate) fn seed_terminal_of(name: &str, socket: Option<&Path>) -> Option<u32> {
    let socket_path = socket.map_or_else(default_socket_path, Path::to_path_buf);
    let selector = phux_client::selector::parse(name).ok()?;
    let rt = cli_runtime().ok()?;
    rt.block_on(async {
        let snapshot = phux_client::state::get_state(&socket_path)
            .await
            .ok()?
            .into_snapshot_ignoring_degradation();
        let ids = crate::commands::resolve_targets(&socket_path, &selector, &snapshot)
            .await
            .ok()?;
        ids.iter()
            .filter_map(phux_protocol::ids::ResourceId::local_id)
            .min()
    })
}
