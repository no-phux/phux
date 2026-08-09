//! `phux worktree` — git worktrees bound to sessions by name (ADR-0054).
//!
//! This is a composition layer, not a subsystem. It shells out to `git
//! worktree` and then reuses the shipped `new` / `ls` / `kill` verbs. The
//! server learns nothing about git and no wire message changes.
//!
//! The binding between a checkout and a session is a **pure function of the
//! worktree path** — [`session_name_for`] — so it can never be stale. There is
//! no mapping table to invalidate when git deletes a worktree behind our back.
//!
//! Every verb carries `--json` (phux-w7z2.34). `new` and `open` return the
//! seed pane's `terminal_id` alongside the branch, path, and session, because
//! a worktree-per-agent fan-out calls `new` first and needs somewhere to send
//! the first prompt. That the server stores no mapping is what makes this
//! cheap: the verb returns what it just created rather than reading back a
//! table it would first have had to write.

use std::collections::BTreeMap;
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

// ---------------------------------------------------------------------------
// the machine surface (phux-w7z2.34)
// ---------------------------------------------------------------------------

/// The `worktree new --json` / `worktree open --json` result document.
///
/// Pure, so the shape a fan-out script actually depends on is unit-testable
/// with no git and no server behind it.
///
/// `terminal_id` is the reason this document exists. A worktree-per-agent
/// fleet is the shape this whole surface is for, and the orchestrator that
/// creates a worktree needs the seed pane to send its first prompt to. Before
/// this it had two options: shell-parse the prose line, or issue a second
/// `phux ls --json` and guess which pane it had just made — and the guess is
/// wrong under precisely the concurrency that makes fan-out worth doing.
///
/// Nothing here is looked up. The session name is a pure function of the path
/// (ADR-0054) and the pane id comes back from the create itself, so this verb
/// returns what it just made rather than storing a mapping and reading it back.
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

/// The `worktree remove --json` result document.
///
/// `killed_session` is the fact a teardown script cannot otherwise recover:
/// `remove` kills a bound session before handing over to git, and whether it
/// had one to kill decides whether the caller still has an agent to reap.
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

/// Print `doc` on stdout, or report the (unreachable) serialization failure
/// on the `--json` contract line.
fn print_json(doc: &serde_json::Value) -> ExitCode {
    match serde_json::to_string_pretty(doc) {
        Ok(rendered) => {
            outln!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(err) => crate::commands::json_err::emit(
            true,
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::JSON_SERIALIZE,
                format!("could not render worktree JSON: {err}"),
                "this is a phux bug; run `phux doctor` and report it",
            ),
            1,
        ),
    }
}

/// Report a failure on whichever channel the verb's `--json` flag selects.
///
/// Under `--json`, one contract line on stderr with stdout left empty
/// (ADR-0065 §4). Without it, the prose these verbs have always printed,
/// byte-for-byte — the messages already carry their own remedy, so nothing is
/// lost by not repeating it. Exit stays `1` either way: a formatting flag must
/// not change what a failure means.
fn fail_json(json: bool, code: &'static str, message: &str, remedy: &str) -> ExitCode {
    if json {
        return crate::commands::json_err::emit(
            true,
            &crate::commands::json_err::CliError::new(code, message.to_owned(), remedy),
            1,
        );
    }
    fail(message)
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

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
    print_json(&serde_json::json!({ "schema_version": 1, "worktrees": rows }))
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

// ---------------------------------------------------------------------------
// new
// ---------------------------------------------------------------------------

/// The resolved arguments of `phux worktree new`.
///
/// Bundled rather than passed loose: the fields are one cohesive request, and
/// threading them individually through the call chain makes every future
/// addition an edit at three sites instead of one — as `--json` would have
/// been (phux-w7z2.34).
struct NewRequest<'a> {
    branch: &'a str,
    path: Option<&'a Path>,
    from: Option<&'a str>,
    session: Option<&'a str>,
    repo: &'a Path,
    socket: Option<PathBuf>,
    attach: bool,
    json: bool,
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

    // Default sibling layout: `<repo-parent>/<repo-name>-<branch>`. Chosen
    // over a nested `.worktrees/` dir because a worktree inside the repo is
    // one `rm -rf` away from taking the checkout with it, and because tools
    // that walk upward from the worktree would find the parent's `.git`.
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

/// Create the session bound to `dest`, attaching only when asked.
///
/// Headless-by-default is the whole point: `worktree new` is called from
/// scripts, keybindings, and agents far more often than from a prompt, and a
/// create verb that tries to seize the terminal fails in every one of those
/// callers. `--attach` opts into the interactive behavior.
/// Everything [`bind_session`] needs. A struct rather than seven positional
/// arguments, for the same reason [`NewRequest`] is one.
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
        return super::new::run_new(
            None,
            Some(name.to_owned()),
            Some(cwd.to_path_buf()),
            socket,
            false,
            command,
            Vec::new(),
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
    if let Err(err) =
        super::server::ensure_server(&socket_path, super::DEFAULT_SESSION_NAME, None, json)
    {
        // Under `--json` the failure stays off stderr, which carries only the
        // error document; the verb reports it there when the dial fails.
        if json {
            tracing::debug!(error = %err, "auto-spawn failed on the --json worktree path");
        } else {
            eprintln!(
                "phux: auto-spawn skipped ({err}). Start a server manually with `phux server`."
            );
        }
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
        BTreeMap::default(),
        None,
        false,
        json,
    )) {
        // The create already told us the seed pane's id; `--json` just stops
        // throwing it away.
        Ok(terminal_id) => emit_binding(json, branch, cwd, name, terminal_id),
        Err(code) => code,
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
        outln!("{session}");
        return ExitCode::SUCCESS;
    }
    print_json(&binding_json(branch, path, session, terminal_id))
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

fn run_open(
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
            return super::attach::run_attach(Some(name), socket);
        }
        if json {
            // Idempotent has to mean idempotent for machines too: the second
            // `open` must return the same document the first one did, seed
            // pane included, or every caller grows an "already running"
            // branch that goes and finds the pane by hand.
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
        outln!("{name}");
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

/// The seed pane of the live session `name`, or `None` when there is not
/// exactly one to name.
///
/// Lowest id wins, and "lowest" is "oldest": ids are handed out in creation
/// order, so for a session `worktree new` made this is the pane it seeded,
/// whatever has been split off it since. A machine caller wants the same
/// answer on every `open` far more than it wants the focused one, which moves.
///
/// Satellite panes are skipped rather than guessed at: their ids are not
/// wire-local `u32`s, and a worktree session is by construction local anyway.
fn seed_terminal_of(name: &str, socket: Option<&Path>) -> Option<u32> {
    let socket_path = socket.map_or_else(default_socket_path, Path::to_path_buf);
    let selector = crate::selector::parse(name).ok()?;
    let rt = cli_runtime().ok()?;
    rt.block_on(async {
        let snapshot = phux_client::state::get_state(&socket_path)
            .await
            .ok()?
            .into_snapshot_ignoring_degradation();
        let ids = crate::commands::resolve_targets(&socket_path, &selector, &snapshot).await;
        ids.iter()
            .filter_map(phux_protocol::ids::TerminalId::local_id)
            .min()
    })
}

// ---------------------------------------------------------------------------
// remove
// ---------------------------------------------------------------------------

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
                return print_json(&removal_json(
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

/// The removals git will refuse later no matter what, refused FIRST.
///
/// Order is the whole point: every check here runs before the session kill,
/// so a command that ends up refusing to do its job has not destroyed a
/// session on the way to refusing. Asking git for the same verdict up front
/// keeps the failure path free of side effects.
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

/// Kill the session bound to this worktree, if one is live, and wait for it
/// to actually be gone. Returns whether there was one to kill.
///
/// This happens BEFORE git, not after: git refuses to remove a worktree whose
/// files are held open, and a shell sitting in that cwd holds it open. The
/// reverse order is the failure users actually hit.
fn kill_bound_session(
    name: &str,
    path: &Path,
    socket: Option<&Path>,
    json: bool,
) -> Result<bool, Refusal> {
    if !live_session_names(socket).is_some_and(|names| names.iter().any(|live| live == name)) {
        return Ok(false);
    }

    if super::kill::run_kill(name, socket.map(Path::to_path_buf)) != ExitCode::SUCCESS {
        return Err(Refusal::workspace(
            format!(
                "could not kill session '{name}' bound to {} — worktree left in place",
                path.display()
            ),
            "kill the session yourself with `phux kill`, then retry",
        ));
    }

    // `kill` returns as soon as the server accepts it, not once the panes are
    // gone — and a shell that still holds this cwd is exactly what makes `git
    // worktree remove` fail. Wait for the session to actually leave the
    // snapshot before handing over to git, so the ordering this command
    // promises is real and not just nominal.
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

/// A failure or refusal, carrying the stable code the `--json` contract line
/// needs alongside the prose the human path prints.
///
/// Kept together deliberately: the two output channels describing the same
/// event must never be able to disagree about which event it was.
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
    // `into_snapshot_ignoring_degradation`: `sessions` never aggregates across
    // a federation (`handle_get_state_federated` drops each satellite's list
    // — the `u32` ids would collide with the hub's), so an unreachable
    // satellite cannot change the set of names this returns.
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

    /// phux-w7z2.34, the reason the document exists: `worktree new --json`
    /// hands back the seed pane's `terminal_id`, so the first call in a
    /// fan-out script does not have to go find the pane it just created.
    ///
    /// The full key set is pinned because this is a frozen surface
    /// (ADR-0071): a consumer reads these names, so losing or renaming one is
    /// a breaking change, not a refactor.
    #[test]
    fn the_new_document_carries_the_seed_terminal_id() {
        let doc = binding_json(
            Some("feat/auth"),
            Path::new("/src/phux-feat-auth"),
            "phux-feat-auth",
            42,
        );
        assert_eq!(doc["schema_version"], serde_json::json!(1));
        assert_eq!(doc["branch"], serde_json::json!("feat/auth"));
        assert_eq!(doc["path"], serde_json::json!("/src/phux-feat-auth"));
        assert_eq!(doc["session"], serde_json::json!("phux-feat-auth"));
        assert_eq!(
            doc["terminal_id"],
            serde_json::json!(42),
            "the terminal_id IS the feature: without it a caller must guess \
             which pane it just made",
        );

        // `serde_json::Map` is ordered by key, so compare the set, not the
        // order — the contract is which fields exist, not how they are laid
        // out in the rendering.
        let keys: Vec<&str> = doc
            .as_object()
            .expect("a JSON object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            vec!["branch", "path", "schema_version", "session", "terminal_id"],
            "the key set is frozen surface (ADR-0071): dropping or renaming \
             one breaks every consumer",
        );
    }

    /// The session in the document is the one ADR-0054 derives from the path,
    /// so a caller can recompute it and a caller that does not have to trust
    /// what it was handed. `-s NAME` is the only thing that breaks the tie,
    /// and then the document reports the override, not the derivation.
    #[test]
    fn the_document_reports_the_session_that_was_actually_bound() {
        let path = Path::new("/src/phux-feat-auth");
        let derived = binding_json(Some("feat/auth"), path, &session_name_for(path), 7);
        assert_eq!(derived["session"], serde_json::json!("phux-feat-auth"));

        let overridden = binding_json(Some("feat/auth"), path, "agent-3", 7);
        assert_eq!(overridden["session"], serde_json::json!("agent-3"));
    }

    /// A detached checkout has no branch, and the document says `null` rather
    /// than inventing a name — `worktree open` can be pointed at one.
    #[test]
    fn a_detached_checkout_reports_a_null_branch() {
        let doc = binding_json(None, Path::new("/src/detached"), "detached", 1);
        assert_eq!(doc["branch"], serde_json::Value::Null);
    }

    /// Teardown has the same parsing problem creation does. `killed_session`
    /// is the one fact the caller cannot recover afterwards: by the time the
    /// document is written the session is gone either way.
    #[test]
    fn the_remove_document_says_whether_a_session_was_killed() {
        let killed = removal_json(
            Some("feat/auth"),
            Path::new("/src/phux-feat-auth"),
            "phux-feat-auth",
            true,
        );
        assert_eq!(killed["schema_version"], serde_json::json!(1));
        assert_eq!(killed["killed_session"], serde_json::json!(true));
        assert_eq!(killed["removed"], serde_json::json!(true));
        assert_eq!(killed["session"], serde_json::json!("phux-feat-auth"));

        let unbound = removal_json(None, Path::new("/src/x"), "x", false);
        assert_eq!(unbound["killed_session"], serde_json::json!(false));
    }

    /// `--json` and `--attach` cannot combine: an attached session owns the
    /// terminal, so there would be no stdout left to put the document on.
    /// Asserted through the parser, because that is where the refusal lives.
    #[test]
    fn json_and_attach_are_mutually_exclusive_on_new_and_open() {
        use clap::Parser as _;

        for args in [
            vec!["phux", "worktree", "new", "b", "--json", "--attach"],
            vec!["phux", "worktree", "open", "b", "--json", "--attach"],
        ] {
            let parsed = crate::Cli::try_parse_from(&args);
            assert!(
                parsed.is_err(),
                "`{}` must be refused at parse time",
                args.join(" ")
            );
        }
    }

    /// The `--json` failure channel is the shared contract line, so a script
    /// that branches on `error.code` gets a code and not prose.
    #[test]
    fn a_json_failure_carries_a_stable_code_and_leaves_stdout_alone() {
        let doc = crate::commands::json_err::error_document(
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::WORKSPACE,
                "not a git repository",
                "run this inside a git repository, or point --repo at one",
            ),
            1,
        );
        assert_eq!(doc["error"]["code"], serde_json::json!("workspace"));
        assert_eq!(doc["exit_code"], serde_json::json!(1));
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
