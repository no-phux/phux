//! `phux worktree` — git worktrees bound to sessions by name (ADR-0054).
//!
//! This is a composition layer, not a subsystem. It shells out to `git
//! worktree` and then reuses the shipped `new` / `ls` / `kill` verbs. The
//! server learns nothing about git and no wire message changes.
//!
//! The binding between a checkout and a session is a **pure function of the
//! worktree path** — [`session_name_for`] — so it can never be stale. There is
//! no mapping table to invalidate when git deletes a worktree behind our back.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_server::runtime::default_socket_path;

use crate::commands::{WorktreeAction, cli_runtime};

use super::workspace::{WorktreeInfo, git_bytes, git_text, parse_worktrees};

/// Fallback session name when a worktree basename sanitizes to nothing.
///
/// Reachable for a checkout in a directory named entirely out of reserved or
/// non-ASCII characters. A predictable fallback beats a failure: the operator
/// can always override with `-s NAME`.
const FALLBACK_NAME: &str = "worktree";

/// Derive the session name bound to a worktree path (ADR-0054).
///
/// Total and deterministic: the same path always yields the same name, on any
/// client, without consulting server state. Characters outside
/// `[A-Za-z0-9._-]` collapse to `-` because the selector grammar reserves `:`
/// and treats a leading `@`/`#` as an id/tag sigil, and a bare `.` is the
/// focused-pane selector.
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

pub(crate) fn run_worktree(action: &WorktreeAction) -> ExitCode {
    match action {
        WorktreeAction::List { path, json, socket } => run_list(path, *json, socket.as_deref()),
        WorktreeAction::New {
            branch,
            path,
            from,
            session,
            repo,
            socket,
            attach,
            command,
        } => run_new(NewRequest {
            branch,
            path: path.as_deref(),
            from: from.as_deref(),
            session: session.as_deref(),
            repo,
            socket: socket.clone(),
            attach: *attach,
            command: command.clone(),
        }),
        WorktreeAction::Open {
            target,
            repo,
            socket,
            attach,
        } => run_open(target, repo, socket.clone(), *attach),
        WorktreeAction::Remove {
            target,
            force,
            repo,
            socket,
        } => run_remove(target, *force, repo, socket.as_deref()),
    }
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

fn run_list(path: &Path, json: bool, socket: Option<&Path>) -> ExitCode {
    let entries = match collect(path) {
        Ok(entries) => entries,
        Err(err) => return fail(&err),
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
    let doc = serde_json::json!({ "schema_version": 1, "worktrees": rows });
    match serde_json::to_string_pretty(&doc) {
        Ok(rendered) => {
            println!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(err) => fail(&format!("could not render worktree JSON: {err}")),
    }
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
        println!(
            "{marker} {:<24} {branch:<24} {} [{bound}]",
            entry.session,
            entry.info.path.display()
        );
    }
}

// ---------------------------------------------------------------------------
// new
// ---------------------------------------------------------------------------

/// The resolved arguments of `phux worktree new`.
///
/// Bundled rather than passed loose: the eight fields are one cohesive
/// request, and threading them individually through the call chain makes
/// every future addition an edit at three sites instead of one.
struct NewRequest<'a> {
    branch: &'a str,
    path: Option<&'a Path>,
    from: Option<&'a str>,
    session: Option<&'a str>,
    repo: &'a Path,
    socket: Option<PathBuf>,
    attach: bool,
    command: Vec<String>,
}

fn run_new(req: NewRequest<'_>) -> ExitCode {
    let NewRequest {
        branch,
        path,
        from,
        session,
        repo,
        socket,
        attach,
        command,
    } = req;
    let root = match repo_root(repo) {
        Ok(root) => root,
        Err(err) => return fail(&err),
    };

    // Default sibling layout: `<repo-parent>/<repo-name>-<branch>`. Chosen
    // over a nested `.worktrees/` dir because a worktree inside the repo is
    // one `rm -rf` away from taking the checkout with it, and because tools
    // that walk upward from the worktree would find the parent's `.git`.
    let dest = path.map_or_else(|| default_worktree_path(&root, branch), Path::to_path_buf);

    if dest.exists() {
        return fail(&format!(
            "{} already exists — pass --path to choose another location",
            dest.display()
        ));
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
        return fail(&format!(
            "session name '{name}' is already derived by worktree {} — pass -s NAME",
            other.path.display()
        ));
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
        return fail(&err);
    }
    println!("worktree {} {branch}", dest.display());

    bind_session(&name, &dest, socket, command, attach)
}

/// Create the session bound to `dest`, attaching only when asked.
///
/// Headless-by-default is the whole point: `worktree new` is called from
/// scripts, keybindings, and agents far more often than from a prompt, and a
/// create verb that tries to seize the terminal fails in every one of those
/// callers. `--attach` opts into the interactive behavior.
fn bind_session(
    name: &str,
    cwd: &Path,
    socket: Option<PathBuf>,
    command: Vec<String>,
    attach: bool,
) -> ExitCode {
    if attach {
        return super::new::run_new(
            None,
            Some(name.to_owned()),
            Some(cwd.to_path_buf()),
            socket,
            false,
            command,
        );
    }

    let socket_path = socket.unwrap_or_else(default_socket_path);
    if let Err(code) = super::ensure_socket_path_fits(&socket_path) {
        return code;
    }
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    // Auto-spawn under the default session name, not the worktree's: the
    // seed session must not collide with the one we are about to create.
    if !socket_path.exists()
        && let Err(err) =
            super::server::maybe_auto_spawn_server(&socket_path, super::DEFAULT_SESSION_NAME, None)
    {
        eprintln!("phux: auto-spawn skipped ({err}). Start a server manually with `phux server`.");
    }

    let command = if command.is_empty() {
        None
    } else {
        Some(command)
    };
    match rt.block_on(super::new::create_session_via_metadata(
        &socket_path,
        name,
        command,
        Some(cwd.to_string_lossy().into_owned()),
    )) {
        Ok(_) => {
            println!("{name}");
            ExitCode::SUCCESS
        }
        Err(code) => code,
    }
}

fn default_worktree_path(root: &Path, branch: &str) -> PathBuf {
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

// ---------------------------------------------------------------------------
// open
// ---------------------------------------------------------------------------

fn run_open(target: &str, repo: &Path, socket: Option<PathBuf>, attach: bool) -> ExitCode {
    let (_root, entry) = match resolve_target(target, repo) {
        Ok(found) => found,
        Err(err) => return fail(&err),
    };
    let name = session_name_for(&entry.path);

    // Already live: report and exit 0 rather than erroring on the duplicate.
    // `open` is idempotent by design — scripts and keybindings call it
    // without checking first. With `--attach`, join the live session.
    if live_session_names(socket.as_deref()).is_some_and(|names| names.contains(&name)) {
        if attach {
            return super::attach::run_attach(Some(name), socket);
        }
        println!("{name}");
        return ExitCode::SUCCESS;
    }

    bind_session(&name, &entry.path, socket, Vec::new(), attach)
}

// ---------------------------------------------------------------------------
// remove
// ---------------------------------------------------------------------------

fn run_remove(target: &str, force: bool, repo: &Path, socket: Option<&Path>) -> ExitCode {
    let (root, entry) = match resolve_target(target, repo) {
        Ok(found) => found,
        Err(err) => return fail(&err),
    };

    if entry.current {
        return fail(
            "refusing to remove the worktree you are standing in — run this from another checkout",
        );
    }

    // Check cleanliness BEFORE killing anything. git makes the same refusal
    // later, but by then the session is already gone — and a command that
    // refuses to do its job must not have destroyed something on the way to
    // refusing. Asking git for the same verdict up front keeps the failure
    // path free of side effects.
    if !force && let Some(dirt) = dirty_summary(&entry.path) {
        return fail(&format!(
            "{} has {dirt} — commit, stash, or pass --force (the session was left running)",
            entry.path.display()
        ));
    }

    let name = session_name_for(&entry.path);

    // Kill the session BEFORE git, not after: git refuses to remove a
    // worktree whose files are held open, and a shell sitting in that cwd
    // holds it open. The reverse order is the failure users actually hit.
    if live_session_names(socket).is_some_and(|names| names.contains(&name)) {
        let code = super::kill::run_kill(&name, socket.map(Path::to_path_buf));
        if code != ExitCode::SUCCESS {
            return fail(&format!(
                "could not kill session '{name}' bound to {} — worktree left in place",
                entry.path.display()
            ));
        }
        // `kill` returns as soon as the server accepts it, not once the
        // panes are gone — and a shell that still holds this cwd is exactly
        // what makes `git worktree remove` fail. Wait for the session to
        // actually leave the snapshot before handing over to git, so the
        // ordering this command promises is real and not just nominal.
        if !wait_for_session_gone(&name, socket) {
            return fail(&format!(
                "session '{name}' did not shut down within {WAIT_FOR_KILL_MS}ms — worktree left in place; retry, or pass --force once its processes exit"
            ));
        }
        println!("killed session {name}");
    }

    let path_str = entry.path.to_string_lossy().into_owned();
    let mut args: Vec<&str> = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&path_str);

    match git_bytes(&root, &args) {
        Ok(_) => {
            println!("removed worktree {}", entry.path.display());
            ExitCode::SUCCESS
        }
        Err(err) => fail(&err),
    }
}

// ---------------------------------------------------------------------------
// shared
// ---------------------------------------------------------------------------

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

/// Resolve a `BRANCH | PATH | SESSION` argument to exactly one worktree.
///
/// Matching is tried most-specific first — path, then branch, then derived
/// session name — so an unambiguous argument never needs a flag to
/// disambiguate it.
fn resolve_target(target: &str, repo: &Path) -> Result<(PathBuf, WorktreeInfo), String> {
    let root = repo_root(repo)?;
    let entries = collect(&root)?;

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
            Err(format!(
                "no worktree matches '{target}' — `phux worktree list` shows the paths, branches, and session names that do"
            ))
        },
        |entry| Ok((root.clone(), entry.clone())),
    )
}

/// Describe what makes `path` dirty, or `None` when it is clean.
///
/// Counts modified and untracked entries separately because they are
/// different mistakes: modified files are work about to be lost, untracked
/// ones are usually build output the operator does not care about.
///
/// A worktree git cannot stat (already deleted, permissions) reads as clean:
/// `git worktree remove` is then the right thing to run and will produce the
/// authoritative error itself. This function only ever *adds* a refusal, so
/// failing open here cannot delete anything git would have protected.
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
///
/// Generous enough for a shell to run its exit traps, short enough that a
/// wedged pane reports rather than hangs a script.
const WAIT_FOR_KILL_MS: u64 = 3000;

/// Poll interval while waiting for teardown. Small relative to the budget so
/// the common case (already gone) costs one round trip.
const WAIT_POLL_MS: u64 = 50;

/// Block until no session named `name` is on the server, or the budget runs
/// out. Returns whether the session is gone.
///
/// A server that has become unreachable counts as gone: there is no session
/// holding the worktree open if there is no server.
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

/// Session names currently on the server, or `None` when none is running.
///
/// `None` is deliberately distinct from an empty set: "no server" and "a
/// server with no sessions" are different facts, and the listing shows them
/// differently (`?` versus `-`).
fn live_session_names(socket: Option<&Path>) -> Option<Vec<String>> {
    let socket_path = socket.map_or_else(default_socket_path, Path::to_path_buf);
    let rt = cli_runtime().ok()?;
    let snapshot = rt
        .block_on(phux_client::state::get_state(&socket_path))
        .ok()?;
    Some(
        snapshot
            .sessions
            .iter()
            .map(|session| session.name.clone())
            .collect(),
    )
}

fn fail(message: &str) -> ExitCode {
    eprintln!("phux: {message}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_name_is_the_sanitized_basename() {
        assert_eq!(
            session_name_for(Path::new("/w/phux-feat-auth")),
            "phux-feat-auth"
        );
        assert_eq!(session_name_for(Path::new("/w/my repo")), "my-repo");
        assert_eq!(session_name_for(Path::new("/w/v1.2_beta")), "v1.2_beta");
    }

    #[test]
    fn derived_name_collapses_dash_runs() {
        assert_eq!(session_name_for(Path::new("/w/a---b")), "a-b");
        // Three separate illegal characters must still collapse to one dash.
        assert_eq!(session_name_for(Path::new("/w/a ~ b")), "a-b");
    }

    #[test]
    fn derived_name_strips_selector_sigils_from_the_edges() {
        // A leading `@` would parse as a terminal id, `#` as a tag, and a
        // bare `.` as the focused-pane selector.
        assert_eq!(session_name_for(Path::new("/w/@42")), "42");
        assert_eq!(session_name_for(Path::new("/w/#tag")), "tag");
        assert_eq!(session_name_for(Path::new("/w/.hidden")), "hidden");
    }

    #[test]
    fn derived_name_never_contains_the_selector_separator() {
        let name = session_name_for(Path::new("/w/work:1.0"));
        assert!(
            !name.contains(':'),
            "derived '{name}' would parse as a locus"
        );
        assert_eq!(name, "work-1.0");
    }

    #[test]
    fn derived_name_falls_back_when_nothing_survives() {
        assert_eq!(session_name_for(Path::new("/w/...")), FALLBACK_NAME);
        assert_eq!(session_name_for(Path::new("/")), FALLBACK_NAME);
    }

    #[test]
    fn derivation_is_stable_across_calls() {
        let path = Path::new("/w/feat/@weird name--here");
        assert_eq!(session_name_for(path), session_name_for(path));
    }

    #[test]
    fn clean_worktree_summarizes_to_nothing() {
        assert_eq!(summarize_porcelain(""), None);
        assert_eq!(summarize_porcelain("\n  \n"), None);
    }

    #[test]
    fn porcelain_separates_modified_from_untracked() {
        // Staged, unstaged, and renamed entries are all "work to lose";
        // only `??` is untracked.
        assert_eq!(
            summarize_porcelain("?? a.txt\n?? b.txt\n"),
            Some("2 untracked file(s)".to_owned())
        );
        assert_eq!(
            summarize_porcelain(" M a.txt\nA  b.txt\nR  c.txt -> d.txt\n"),
            Some("3 modified file(s)".to_owned())
        );
        assert_eq!(
            summarize_porcelain(" M a.txt\n?? b.txt\n"),
            Some("1 modified and 1 untracked file(s)".to_owned())
        );
    }

    #[test]
    fn default_path_is_a_sibling_of_the_repo() {
        let dest = default_worktree_path(Path::new("/src/phux"), "feat/auth");
        assert_eq!(dest, PathBuf::from("/src/phux-feat-auth"));
    }

    #[test]
    fn default_path_flattens_slashes_in_the_branch() {
        let dest = default_worktree_path(Path::new("/src/phux"), "a/b/c");
        assert_eq!(dest, PathBuf::from("/src/phux-a-b-c"));
        assert_eq!(dest.components().count(), 3);
    }
}
