//! Git helpers shared by `phux project` and `phux worktree`: run `git` in a
//! directory and parse `git worktree list --porcelain -z`.

use std::path::{Path, PathBuf};
use std::process::Command;

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
    // git's own severity word reads as noise after our `phux:` prefix.
    let detail = stderr.trim();
    let detail = detail.strip_prefix("fatal: ").unwrap_or(detail);
    if detail.is_empty() {
        Err(format!("git {args:?} failed"))
    } else {
        Err(detail.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// git's `fatal:` severity word is dropped, so the error reads
    /// `phux: not a git repository ...`, not `phux: fatal: ...`.
    #[test]
    fn git_errors_drop_the_fatal_prefix() {
        let dir = tempfile::tempdir().expect("tempdir");
        let Err(err) = git_text(dir.path(), &["rev-parse", "--show-toplevel"]) else {
            // Inside some enclosing repository: nothing to check.
            return;
        };
        assert!(!err.starts_with("fatal:"), "{err}");
        assert!(err.contains("not a git repository"), "{err}");
    }

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
