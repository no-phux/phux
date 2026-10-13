//! `phux project inspect` — map a git repository and its worktrees. A local
//! repo operation: it never contacts a server and never creates or deletes a
//! worktree.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::commands::git::{WorktreeInfo, git_bytes, git_text, parse_worktrees};

const SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
struct InspectReport {
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

pub(crate) fn run_inspect(path: &Path, json: bool) -> ExitCode {
    match inspect_repo(path) {
        Ok(report) if json => print_json(&report),
        Ok(report) => print_human(&report),
        // The shared error contract (phux-i0e8.8.3): one JSON document under
        // `--json`, the message and its remedy as prose otherwise.
        Err(err) => crate::commands::json_err::emit(
            json,
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::WORKSPACE,
                err,
                "run this inside a git repository, or pass a path to one",
            ),
            1,
        ),
    }
}

fn inspect_repo(path: &Path) -> Result<InspectReport, String> {
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
    Ok(InspectReport {
        repo: RepoInfo {
            path: repo.path,
            head: repo.head,
            branch: repo.branch,
            detached: repo.detached,
        },
        worktrees,
    })
}

fn print_json(report: &InspectReport) -> ExitCode {
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
    crate::output::json(&doc)
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

fn print_human(report: &InspectReport) -> ExitCode {
    let branch = report.repo.branch.as_deref().unwrap_or("(detached)");
    outln!("repo {} {branch}", report.repo.path.display());
    for worktree in &report.worktrees {
        let marker = if worktree.current { "*" } else { " " };
        let state = worktree.branch.as_deref().unwrap_or("(detached)");
        let head = worktree.head.as_deref().unwrap_or("-");
        outln!("{marker} {} {state} {head}", worktree.path.display());
    }
    ExitCode::SUCCESS
}
