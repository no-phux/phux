//! `phux worktree` — git worktrees bound to sessions by name (ADR-0054).
//!
//! A composition layer: it shells out to `git worktree` and reuses `new` / `ls`
//! / `kill`; the server learns nothing about git. The binding is a pure
//! function of the worktree path ([`session_name_for`]), so it is never stale.
//! Every verb carries `--json`; `new` and `open` return the seed pane's
//! `terminal_id` so a fan-out script can send its first prompt.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_server::runtime::default_socket_path;

use crate::commands::{WorktreeAction, cli_runtime};

use super::git::{WorktreeInfo, git_bytes, git_text, parse_worktrees};
use session::{NewRequest, run_new, run_open};

#[expect(
    unused_imports,
    reason = "preserve the crate-facing worktree API after extracting session lifecycle"
)]
pub(crate) use session::{TaskSession, create_task_worktree, seed_terminal_of};

mod session;

/// Fallback session name when a worktree basename sanitizes to nothing;
/// `-s NAME` overrides.
const FALLBACK_NAME: &str = "worktree";

/// Derive the session name bound to a worktree path (ADR-0054):
/// deterministic, with characters outside `[A-Za-z0-9._-]` collapsed to `-`
/// because the selector grammar reserves `:`, leading `@`/`#`, and a bare `.`.
#[must_use]
pub(crate) fn session_name_for(path: &Path) -> String {
    let base = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());

    let mut out = String::with_capacity(base.len());
    let mut last_was_dash = false;
    for ch in base.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            // Collapse runs of `-` so `feat--foo` and `feat/@foo` do not
            // produce two different names for what reads as one label.
            if ch == '-' {
                if last_was_dash {
                    continue;
                }
                last_was_dash = true;
            } else {
                last_was_dash = false;
            }
            out.push(ch);
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }

    // Strip selector sigils and separators from the edges. A name that starts
    // with `@` or `#` parses as a terminal id or a tag, not a session.
    let trimmed = out.trim_matches(|c| c == '-' || c == '.' || c == '@' || c == '#' || c == '=');
    if trimmed.is_empty() {
        return FALLBACK_NAME.to_owned();
    }
    trimmed.to_owned()
}

/// One worktree plus the session-binding view of it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BoundWorktree {
    info: WorktreeInfo,
    session: String,
    /// Whether a session by that name exists right now. `None` when no
    /// server is running, which is a different fact from "not bound".
    bound: Option<bool>,
}

pub(crate) fn run_worktree(action: &WorktreeAction, socket: Option<PathBuf>) -> ExitCode {
    match action {
        WorktreeAction::List { path, json } => run_list(path, *json, socket.as_deref()),
        WorktreeAction::New {
            branch,
            path,
            from,
            session,
            repo,
            attach,
            json,
            command,
        } => run_new(NewRequest {
            branch,
            path: path.as_deref(),
            from: from.as_deref(),
            session: session.as_deref(),
            repo,
            socket,
            attach: *attach,
            json: *json,
            command: command.clone(),
        }),
        WorktreeAction::Open {
            target,
            repo,
            attach,
            json,
        } => run_open(target, repo, socket, *attach, *json),
        WorktreeAction::Remove {
            target,
            force,
            repo,
            json,
        } => run_remove(target, *force, repo, socket.as_deref(), *json),
    }
}

/// The `worktree new --json` / `open --json` document. `terminal_id` is the
/// point: the orchestrator needs the seed pane without a racy `ls` lookup.
fn binding_json(
    branch: Option<&str>,
    path: &Path,
    session: &str,
    terminal_id: u64,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "branch": branch,
        "path": path,
        "session": session,
        "terminal_id": terminal_id,
    })
}

/// The `worktree remove --json` document. `killed_session` tells a teardown
/// script whether there was an agent to reap.
fn removal_json(
    branch: Option<&str>,
    path: &Path,
    session: &str,
    killed_session: bool,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "branch": branch,
        "path": path,
        "session": session,
        "killed_session": killed_session,
        "removed": true,
    })
}

/// Report a failure: under `--json` one contract line on stderr with stdout
/// empty (ADR-0065 §4), otherwise the historical prose. Exit is `1` either way.
fn fail_json(json: bool, code: &'static str, message: &str, remedy: &str) -> ExitCode {
    crate::commands::json_err::emit(
        json,
        &crate::commands::json_err::CliError::new(code, message.to_owned(), remedy),
        1,
    )
}

fn run_list(path: &Path, json: bool, socket: Option<&Path>) -> ExitCode {
    let entries = match collect(path) {
        Ok(entries) => entries,
        // Every worktree verb now carries `--json` (phux-w7z2.34), and they
        // all report failure through the one shared contract (phux-i0e8.8.3).
        // The prose spelling is unchanged.
        Err(err) => {
            return fail_json(
                json,
                crate::commands::json_err::codes::WORKSPACE,
                &err,
                "run this inside a git repository, or pass a path to one",
            );
        }
    };
    // A missing server is not a listing failure — the worktrees are still
    // real. `bound` degrades to `None` and the human view says "-".
    let live = live_session_names(socket);
    let bound: Vec<BoundWorktree> = entries
        .into_iter()
        .map(|info| {
            let session = session_name_for(&info.path);
            let bound = live.as_ref().map(|names| names.contains(&session));
            BoundWorktree {
                info,
                session,
                bound,
            }
        })
        .collect();

    if json {
        print_list_json(&bound)
    } else {
        print_list_human(&bound);
        ExitCode::SUCCESS
    }
}

fn print_list_json(entries: &[BoundWorktree]) -> ExitCode {
    let rows: Vec<_> = entries
        .iter()
        .map(|entry| {
            serde_json::json!({
                "path": entry.info.path,
                "branch": entry.info.branch,
                "head": entry.info.head,
                "detached": entry.info.detached,
                "current": entry.info.current,
                "session": entry.session,
                "bound": entry.bound,
            })
        })
        .collect();
    crate::output::json(&serde_json::json!({ "schema_version": 1, "worktrees": rows }))
}

fn print_list_human(entries: &[BoundWorktree]) {
    for entry in entries {
        let marker = if entry.info.current { "*" } else { " " };
        let branch = entry.info.branch.as_deref().unwrap_or("(detached)");
        let bound = match entry.bound {
            Some(true) => "live",
            Some(false) => "-",
            None => "?",
        };
        outln!(
            "{marker} {:<24} {branch:<24} {} [{bound}]",
            entry.session,
            entry.info.path.display()
        );
    }
}

fn run_remove(
    target: &str,
    force: bool,
    repo: &Path,
    socket: Option<&Path>,
    json: bool,
) -> ExitCode {
    let (root, entry) = match resolve_target(target, repo) {
        Ok(found) => found,
        Err(err) => return report(json, &err),
    };

    if let Some(refusal) = refuse_doomed_removal(&entry, force) {
        return report(json, &refusal);
    }

    let name = session_name_for(&entry.path);
    let killed_session = match kill_bound_session(&name, &entry.path, socket, json) {
        Ok(killed) => killed,
        Err(refusal) => return report(json, &refusal),
    };

    let path_str = entry.path.to_string_lossy().into_owned();
    let mut args: Vec<&str> = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&path_str);

    match git_bytes(&root, &args) {
        Ok(_) => {
            if json {
                return crate::output::json(&removal_json(
                    entry.branch.as_deref(),
                    &entry.path,
                    &name,
                    killed_session,
                ));
            }
            outln!("removed worktree {}", entry.path.display());
            ExitCode::SUCCESS
        }
        // The session is already gone by here, and the caller needs to know
        // that even though the removal failed — so the remedy says it and the
        // exit code still reports failure.
        Err(err) => fail_json(
            json,
            crate::commands::json_err::codes::WORKSPACE,
            &err,
            if killed_session {
                "git refused the removal after the bound session was already killed"
            } else {
                "git refused the removal; the message above is git's own"
            },
        ),
    }
}

/// Refusals git would issue anyway, checked BEFORE the session kill so a
/// refused remove has no side effects.
fn refuse_doomed_removal(entry: &WorktreeInfo, force: bool) -> Option<Refusal> {
    if entry.current {
        return Some(Refusal::workspace(
            "refusing to remove the worktree you are standing in — run this from another checkout",
            "run this from another checkout",
        ));
    }
    if entry.is_main {
        return Some(Refusal::workspace(
            "refusing to remove the main working tree — git will not remove it either",
            "remove one of the repository's other worktrees instead",
        ));
    }
    if entry.locked {
        return Some(Refusal::workspace(
            format!(
                "{} is locked — run `git worktree unlock` first (the session was left running)",
                entry.path.display()
            ),
            "run `git worktree unlock` and retry",
        ));
    }
    if !force && let Some(dirt) = dirty_summary(&entry.path) {
        return Some(Refusal::workspace(
            format!(
                "{} has {dirt} — commit, stash, or pass --force (the session was left running)",
                entry.path.display()
            ),
            "commit or stash the work, or pass --force to discard it",
        ));
    }
    None
}

/// Kill the session bound to this worktree and wait until it is gone;
/// returns whether there was one. Runs before git, which refuses to remove a
/// worktree a shell still holds open.
fn kill_bound_session(
    name: &str,
    path: &Path,
    socket: Option<&Path>,
    json: bool,
) -> Result<bool, Refusal> {
    if !live_session_names(socket).is_some_and(|names| names.iter().any(|live| live == name)) {
        return Ok(false);
    }

    // `phux worktree rm` names this worktree, and removing it means ending
    // the session bound to it: the verb itself is the consent the dangerous
    // kill needs (ADR-0128), so it does not ask again.
    if super::kill::run_kill(
        name,
        None,
        super::server_target::ServerSpec::local(socket.map(Path::to_path_buf)),
        |_| Ok(()),
    ) != ExitCode::SUCCESS
    {
        return Err(Refusal::workspace(
            format!(
                "could not kill session '{name}' bound to {} — worktree left in place",
                path.display()
            ),
            "kill the session yourself with `phux kill`, then retry",
        ));
    }

    // `kill` returns once accepted, not once the panes are gone; wait so git
    // does not find the cwd still held.
    if !wait_for_session_gone(name, socket) {
        return Err(Refusal::workspace(
            format!(
                "session '{name}' did not shut down within {WAIT_FOR_KILL_MS}ms — worktree left in place; retry, or pass --force once its processes exit"
            ),
            "retry once the session's processes exit",
        ));
    }

    if !json {
        outln!("killed session {name}");
    }
    Ok(true)
}

/// Every worktree of the repo containing `path`.
fn collect(path: &Path) -> Result<Vec<WorktreeInfo>, String> {
    let root = repo_root(path)?;
    let output = git_bytes(&root, &["worktree", "list", "--porcelain", "-z"])?;
    parse_worktrees(&output, &root)
}

fn repo_root(path: &Path) -> Result<PathBuf, String> {
    let root = git_text(path, &["rev-parse", "--show-toplevel"])?;
    PathBuf::from(root.trim())
        .canonicalize()
        .map_err(|err| format!("could not canonicalize git worktree {}: {err}", root.trim()))
}

/// Resolve a `BRANCH | PATH | SESSION` argument to exactly one worktree,
/// trying path, then branch, then derived session name.
fn resolve_target(target: &str, repo: &Path) -> Result<(PathBuf, WorktreeInfo), Refusal> {
    // The two ways this fails are different failures and a machine caller
    // must be able to tell them apart: "you are not in a git repository" is
    // not "that worktree does not exist", and the remedies share nothing.
    const NOT_A_REPO: &str = "run this inside a git repository, or point --repo at one";
    let root = repo_root(repo).map_err(|err| Refusal::workspace(err, NOT_A_REPO))?;
    let entries = collect(&root).map_err(|err| Refusal::workspace(err, NOT_A_REPO))?;

    let canonical = Path::new(target).canonicalize().ok();
    let found = entries
        .iter()
        .find(|entry| {
            canonical
                .as_ref()
                .is_some_and(|want| entry.path.canonicalize().ok().as_ref() == Some(want))
        })
        .or_else(|| {
            entries
                .iter()
                .find(|entry| entry.branch.as_deref() == Some(target))
        })
        .or_else(|| {
            entries
                .iter()
                .find(|entry| session_name_for(&entry.path) == target)
        });

    found.map_or_else(
        || {
            Err(Refusal {
                code: crate::commands::json_err::codes::NO_SUCH_TARGET,
                message: format!(
                    "no worktree matches '{target}' — `phux worktree list` shows the paths, branches, and session names that do"
                ),
                remedy: "`phux worktree list` shows the paths, branches, and session names that resolve".to_owned(),
            })
        },
        |entry| Ok((root.clone(), entry.clone())),
    )
}

/// A failure with its `--json` code and its prose, kept together so the two
/// channels cannot disagree.
struct Refusal {
    code: &'static str,
    message: String,
    remedy: String,
}

impl Refusal {
    /// The repository or the checkout itself is the problem — not a git
    /// repository, git refused, a worktree that cannot be removed. Distinct
    /// from a miss: this is not "no such target".
    fn workspace(message: impl Into<String>, remedy: &str) -> Self {
        Self {
            code: crate::commands::json_err::codes::WORKSPACE,
            message: message.into(),
            remedy: remedy.to_owned(),
        }
    }
}

/// Report a [`Refusal`] on the channel `json` selects.
fn report(json: bool, err: &Refusal) -> ExitCode {
    fail_json(json, err.code, &err.message, &err.remedy)
}

/// Describe what makes `path` dirty (modified and untracked counted
/// separately), or `None` when clean. An unstattable worktree reads as clean:
/// this only ever adds a refusal, and git reports the real error.
fn dirty_summary(path: &Path) -> Option<String> {
    let output = git_bytes(path, &["status", "--porcelain"]).ok()?;
    let text = String::from_utf8(output).ok()?;
    summarize_porcelain(&text)
}

/// The pure half of [`dirty_summary`]: count `git status --porcelain` lines.
fn summarize_porcelain(text: &str) -> Option<String> {
    let mut modified = 0_usize;
    let mut untracked = 0_usize;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        if line.starts_with("??") {
            untracked = untracked.saturating_add(1);
        } else {
            modified = modified.saturating_add(1);
        }
    }

    match (modified, untracked) {
        (0, 0) => None,
        (0, u) => Some(format!("{u} untracked file(s)")),
        (m, 0) => Some(format!("{m} modified file(s)")),
        (m, u) => Some(format!("{m} modified and {u} untracked file(s)")),
    }
}

/// How long `remove` waits for a killed session to leave the snapshot.
const WAIT_FOR_KILL_MS: u64 = 3000;

/// Poll interval while waiting for teardown. Small relative to the budget so
/// the common case (already gone) costs one round trip.
const WAIT_POLL_MS: u64 = 50;

/// Block until no session named `name` exists or the budget runs out. An
/// unreachable server counts as gone.
fn wait_for_session_gone(name: &str, socket: Option<&Path>) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(WAIT_FOR_KILL_MS);
    loop {
        match live_session_names(socket) {
            None => return true,
            Some(names) if !names.iter().any(|live| live == name) => return true,
            Some(_) => {}
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(WAIT_POLL_MS));
    }
}

/// Session names on the server, or `None` when no server runs (shown as `?`
/// versus `-`).
pub(crate) fn live_session_names(socket: Option<&Path>) -> Option<Vec<String>> {
    let socket_path = socket.map_or_else(default_socket_path, Path::to_path_buf);
    let rt = cli_runtime().ok()?;
    // `sessions` never aggregates across a federation, so a degraded view is
    // still exact here.
    let snapshot = rt
        .block_on(phux_client::state::get_state(&socket_path))
        .ok()?
        .into_snapshot_ignoring_degradation();
    Some(
        snapshot
            .sessions
            .iter()
            .map(|session| session.name.clone())
            .collect(),
    )
}

#[cfg(test)]
mod tests;
