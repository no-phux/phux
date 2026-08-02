use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::commands::WorkspaceAction;

mod archive;

const SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceReport {
    repo: RepoInfo,
    worktrees: Vec<WorktreeInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RepoInfo {
    path: PathBuf,
    head: Option<String>,
    branch: Option<String>,
    detached: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent facts read off `git worktree list --porcelain`, not a state machine"
)]
pub(crate) struct WorktreeInfo {
    pub(crate) path: PathBuf,
    pub(crate) head: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) detached: bool,
    pub(crate) current: bool,
    /// The primary checkout — always listed first by `git worktree list`.
    pub(crate) is_main: bool,
    pub(crate) locked: bool,
}

#[derive(Default)]
struct WorktreeBuilder {
    path: Option<PathBuf>,
    head: Option<String>,
    branch: Option<String>,
    detached: bool,
    locked: bool,
}

pub(crate) fn run_workspace(action: &WorkspaceAction, socket: Option<PathBuf>) -> ExitCode {
    match action {
        WorkspaceAction::Inspect { path, json } => run_inspect(path, *json),
        WorkspaceAction::Save { output } => archive::run_save(socket, output.as_ref()),
        WorkspaceAction::Restore { archive } => archive::run_restore(archive, socket),
    }
}

fn run_inspect(path: &Path, json: bool) -> ExitCode {
    match inspect_workspace(path) {
        Ok(report) if json => print_json(&report),
        Ok(report) => print_human(&report),
        // The `--json`-bearing inspect verb adopts the shared error contract
        // on its failure path (phux-i0e8.8.3); prose is unchanged.
        Err(err) if json => crate::commands::json_err::emit(
            true,
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::WORKSPACE,
                err,
                "run this inside a git repository, or pass a path to one",
            ),
            1,
        ),
        Err(err) => fail(&err),
    }
}

fn inspect_workspace(path: &Path) -> Result<WorkspaceReport, String> {
    let root = git_text(path, &["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(root.trim())
        .canonicalize()
        .map_err(|err| format!("could not canonicalize git worktree {}: {err}", root.trim()))?;
    let worktree_output = git_bytes(&root, &["worktree", "list", "--porcelain", "-z"])?;
    let worktrees = parse_worktrees(&worktree_output, &root)?;
    let Some(repo) = worktrees.iter().find(|entry| entry.current).cloned() else {
        return Err(format!(
            "git worktree list did not include current worktree {}",
            root.display()
        ));
    };
    Ok(WorkspaceReport {
        repo: RepoInfo {
            path: repo.path,
            head: repo.head,
            branch: repo.branch,
            detached: repo.detached,
        },
        worktrees,
    })
}

pub(crate) fn parse_worktrees(
    input: &[u8],
    current_root: &Path,
) -> Result<Vec<WorktreeInfo>, String> {
    let mut entries = Vec::new();
    let mut builder = WorktreeBuilder::default();
    for field in input.split(|byte| *byte == 0) {
        if field.is_empty() {
            flush_worktree(&mut entries, &mut builder, current_root);
            continue;
        }
        let text = std::str::from_utf8(field)
            .map_err(|err| format!("git worktree output was not UTF-8: {err}"))?;
        if let Some(path) = text.strip_prefix("worktree ") {
            flush_worktree(&mut entries, &mut builder, current_root);
            builder.path = Some(PathBuf::from(path));
        } else if let Some(head) = text.strip_prefix("HEAD ") {
            builder.head = Some(head.to_owned());
        } else if let Some(branch) = text.strip_prefix("branch ") {
            builder.branch = Some(short_branch(branch));
        } else if text == "detached" {
            builder.detached = true;
        } else if text == "locked" || text.starts_with("locked ") {
            builder.locked = true;
        }
    }
    flush_worktree(&mut entries, &mut builder, current_root);
    if entries.is_empty() {
        return Err("git worktree list returned no worktrees".to_owned());
    }
    Ok(entries)
}

fn flush_worktree(
    entries: &mut Vec<WorktreeInfo>,
    builder: &mut WorktreeBuilder,
    current_root: &Path,
) {
    let Some(path) = builder.path.take() else {
        return;
    };
    let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
    let current = canonical == current_root;
    entries.push(WorktreeInfo {
        path,
        head: builder.head.take(),
        branch: builder.branch.take(),
        detached: builder.detached,
        current,
        is_main: entries.is_empty(),
        locked: builder.locked,
    });
    builder.detached = false;
    builder.locked = false;
}

fn short_branch(branch: &str) -> String {
    branch
        .strip_prefix("refs/heads/")
        .unwrap_or(branch)
        .to_owned()
}

pub(crate) fn git_text(path: &Path, args: &[&str]) -> Result<String, String> {
    let bytes = git_bytes(path, args)?;
    String::from_utf8(bytes).map_err(|err| format!("git output was not UTF-8: {err}"))
}

pub(crate) fn git_bytes(path: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|err| format!("could not run git: {err}"))?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    if detail.is_empty() {
        Err(format!("git {args:?} failed"))
    } else {
        Err(detail.to_owned())
    }
}

fn print_json(report: &WorkspaceReport) -> ExitCode {
    let worktrees: Vec<_> = report.worktrees.iter().map(worktree_json).collect();
    let doc = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "repo": {
            "path": report.repo.path,
            "head": report.repo.head,
            "branch": report.repo.branch,
            "detached": report.repo.detached,
        },
        "worktrees": worktrees,
    });
    match serde_json::to_string_pretty(&doc) {
        Ok(rendered) => {
            outln!("{rendered}");
            ExitCode::SUCCESS
        }
        // A `--json` path, so the failure is the contract line, never prose.
        Err(err) => crate::commands::json_err::emit(
            true,
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::JSON_SERIALIZE,
                format!("could not render workspace JSON: {err}"),
                "this is a phux bug; run `phux doctor` and report it",
            ),
            1,
        ),
    }
}

fn worktree_json(entry: &WorktreeInfo) -> serde_json::Value {
    serde_json::json!({
        "path": entry.path,
        "head": entry.head,
        "branch": entry.branch,
        "detached": entry.detached,
        "current": entry.current,
    })
}

fn print_human(report: &WorkspaceReport) -> ExitCode {
    let branch = report.repo.branch.as_deref().unwrap_or("(detached)");
    outln!("workspace {} {branch}", report.repo.path.display());
    for worktree in &report.worktrees {
        let marker = if worktree.current { "*" } else { " " };
        let state = worktree.branch.as_deref().unwrap_or("(detached)");
        let head = worktree.head.as_deref().unwrap_or("-");
        outln!("{marker} {} {state} {head}", worktree.path.display());
    }
    ExitCode::SUCCESS
}

fn fail(message: &str) -> ExitCode {
    eprintln!("phux: {message}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_worktree_records() {
        let input = b"worktree /repo\0HEAD abc\0branch refs/heads/main\0\0worktree /repo-detached\0HEAD def\0detached\0locked reason\0\0";
        let worktrees = parse_worktrees(input, Path::new("/repo")).expect("parse worktrees");
        assert_eq!(worktrees.len(), 2);
        assert_eq!(worktrees[0].branch.as_deref(), Some("main"));
        assert!(worktrees[0].current);
        assert!(worktrees[0].is_main);
        assert!(!worktrees[0].locked);
        assert!(worktrees[1].detached);
        assert!(!worktrees[1].is_main);
        assert!(worktrees[1].locked);
        assert_eq!(worktrees[1].branch, None);
    }
}
