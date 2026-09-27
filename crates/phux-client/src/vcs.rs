//! Client-side VCS branch inference for the sidebar's branch line.
//!
//! Derived from a pane's cwd (already on the wire) by walking up to the
//! enclosing `.git`, following worktree gitfiles, and reading `HEAD`: a
//! cached file read, never a `git` subprocess. [`BranchCache`] re-validates
//! only after a short TTL, and re-parses only when `HEAD`'s mtime changed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How long a cached per-cwd answer is served without re-checking the
/// filesystem: bounds the stat rate while keeping a `git switch` visible.
const REVALIDATE_TTL: Duration = Duration::from_secs(2);

/// Length of the short hash rendered for a detached `HEAD`.
const SHORT_HASH_LEN: usize = 8;

/// One memoized answer for a cwd.
#[derive(Debug, Clone)]
struct CacheEntry {
    /// The `HEAD` file the answer was derived from; `None` when the cwd is
    /// not inside a git repository (a negative entry).
    head_path: Option<PathBuf>,
    /// `HEAD`'s mtime at derivation time (when the filesystem reports one).
    head_mtime: Option<SystemTime>,
    /// The derived branch label.
    branch: Option<String>,
    /// When this entry was last validated against the filesystem.
    checked_at: Instant,
}

/// Per-cwd branch memo. Owned by the attach driver; queried at chrome
/// refresh time.
#[derive(Debug, Default)]
pub struct BranchCache {
    entries: HashMap<PathBuf, CacheEntry>,
}

impl BranchCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The branch label for `cwd`, or `None` when it is not inside a git
    /// repository. Serves the memoized answer within the TTL; after that,
    /// re-reads `HEAD` only if its mtime changed (and re-walks from `cwd`
    /// when the repo disappeared or was never found).
    pub fn branch_for(&mut self, cwd: &Path) -> Option<String> {
        let now = Instant::now();
        if let Some(entry) = self.entries.get_mut(&cwd.to_path_buf()) {
            if now.duration_since(entry.checked_at) < REVALIDATE_TTL {
                return entry.branch.clone();
            }
            // TTL expired: cheap re-validation. A positive entry whose HEAD
            // mtime is unchanged keeps its answer without re-parsing.
            if let Some(head) = entry.head_path.clone() {
                let mtime = mtime_of(&head);
                if mtime.is_some() && mtime == entry.head_mtime {
                    entry.checked_at = now;
                    return entry.branch.clone();
                }
            }
        }
        // Miss, negative entry, or stale positive: full (still cheap) walk.
        let derived = derive(cwd);
        let entry = CacheEntry {
            head_mtime: derived
                .as_ref()
                .and_then(|(head, _)| mtime_of(head.as_path())),
            head_path: derived.as_ref().map(|(head, _)| head.clone()),
            branch: derived.map(|(_, branch)| branch),
            checked_at: now,
        };
        let branch = entry.branch.clone();
        self.entries.insert(cwd.to_path_buf(), entry);
        branch
    }
}

/// mtime of `path`, when the platform reports one.
fn mtime_of(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// Uncached inference: the `(HEAD path, branch label)` for `cwd`, or
/// `None` when no enclosing git repository is found.
fn derive(cwd: &Path) -> Option<(PathBuf, String)> {
    let head = head_file_for(cwd)?;
    let contents = std::fs::read_to_string(&head).ok()?;
    let branch = parse_head(&contents)?;
    Some((head, branch))
}

/// Walk up from `cwd` to the first ancestor holding a `.git` entry and
/// resolve it to that repository's `HEAD` file. Handles both the plain
/// `.git/` directory and the worktree `.git` *file* (`gitdir: <path>`).
fn head_file_for(cwd: &Path) -> Option<PathBuf> {
    for dir in cwd.ancestors() {
        let dot_git = dir.join(".git");
        let Ok(meta) = std::fs::symlink_metadata(&dot_git) else {
            continue;
        };
        let git_dir = if meta.is_dir() {
            dot_git
        } else {
            // Worktree gitfile: a one-line `gitdir: <path>` pointer to the
            // per-worktree dir (which holds its own HEAD). A relative
            // pointer is resolved against the directory holding `.git`.
            let contents = std::fs::read_to_string(&dot_git).ok()?;
            let pointer = contents.strip_prefix("gitdir:")?.trim();
            if pointer.is_empty() {
                return None;
            }
            let target = PathBuf::from(pointer);
            if target.is_absolute() {
                target
            } else {
                dir.join(target)
            }
        };
        let head = git_dir.join("HEAD");
        return head.is_file().then_some(head);
    }
    None
}

/// Parse the first line of a `HEAD` file into a display label:
/// `ref: refs/heads/<branch>` yields `<branch>` (slashes in the branch
/// name preserved); any other ref yields its last path component; a
/// detached commit hash yields its first [`SHORT_HASH_LEN`] characters.
fn parse_head(contents: &str) -> Option<String> {
    let line = contents.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    if let Some(reference) = line.strip_prefix("ref:") {
        let reference = reference.trim();
        let branch = reference
            .strip_prefix("refs/heads/")
            .unwrap_or_else(|| reference.rsplit('/').next().unwrap_or(reference));
        return (!branch.is_empty()).then(|| branch.to_owned());
    }
    // Detached HEAD: a bare object hash. Anything that isn't hex is a
    // malformed HEAD we refuse to label rather than mislabel.
    if line.len() >= SHORT_HASH_LEN && line.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(line[..SHORT_HASH_LEN].to_owned());
    }
    None
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    /// Build a fixture repo dir with `.git/HEAD` holding `head_contents`.
    fn fixture_repo(head_contents: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let git = dir.path().join(".git");
        std::fs::create_dir(&git).expect("mkdir .git");
        std::fs::write(git.join("HEAD"), head_contents).expect("write HEAD");
        dir
    }

    #[test]
    fn head_contents_map_to_labels() {
        for (head, label) in [
            ("ref: refs/heads/main\n", Some("main")),
            (
                "ref: refs/heads/wave2/herdr-sidebar\n",
                Some("wave2/herdr-sidebar"),
            ),
            (
                "6eaca20deadbeef00112233445566778899aabb\n",
                Some("6eaca20d"),
            ),
            ("not a ref and not a hash\n", None),
        ] {
            let repo = fixture_repo(head);
            assert_eq!(
                BranchCache::new().branch_for(repo.path()).as_deref(),
                label,
                "{head:?}"
            );
        }
    }

    #[test]
    fn subdirectory_resolves_to_the_enclosing_repo() {
        let repo = fixture_repo("ref: refs/heads/feature\n");
        let nested = repo.path().join("src").join("deep");
        std::fs::create_dir_all(&nested).expect("mkdir nested");
        assert_eq!(
            BranchCache::new().branch_for(&nested),
            Some("feature".to_owned())
        );
    }

    #[test]
    fn non_repo_directory_yields_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(BranchCache::new().branch_for(dir.path()), None);
    }

    #[test]
    fn worktree_gitfile_resolves_to_the_pointed_head() {
        // Layout: `main/.git/worktrees/wt` holds the worktree HEAD;
        // `wt/.git` is a file pointing at it (absolute pointer, the form
        // `git worktree add` writes).
        let root = tempfile::tempdir().expect("tempdir");
        let wt_git_dir = root.path().join("main/.git/worktrees/wt");
        std::fs::create_dir_all(&wt_git_dir).expect("mkdir worktree gitdir");
        std::fs::write(wt_git_dir.join("HEAD"), "ref: refs/heads/wt-branch\n")
            .expect("write worktree HEAD");
        let wt = root.path().join("wt");
        std::fs::create_dir(&wt).expect("mkdir worktree");
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", wt_git_dir.display()),
        )
        .expect("write gitfile");
        assert_eq!(
            BranchCache::new().branch_for(&wt),
            Some("wt-branch".to_owned())
        );
    }

    #[test]
    fn relative_gitfile_pointer_resolves_against_the_worktree() {
        let root = tempfile::tempdir().expect("tempdir");
        let wt = root.path().join("wt");
        let gitdir = wt.join("gitstate");
        std::fs::create_dir_all(&gitdir).expect("mkdir gitstate");
        std::fs::write(gitdir.join("HEAD"), "ref: refs/heads/rel\n").expect("write HEAD");
        std::fs::write(wt.join(".git"), "gitdir: gitstate\n").expect("write gitfile");
        assert_eq!(BranchCache::new().branch_for(&wt), Some("rel".to_owned()));
    }

    #[test]
    fn cache_serves_within_ttl_without_reparsing() {
        let repo = fixture_repo("ref: refs/heads/main\n");
        let mut cache = BranchCache::new();
        assert_eq!(cache.branch_for(repo.path()), Some("main".to_owned()));
        // Within the TTL the memo is served as-is.
        std::fs::write(repo.path().join(".git/HEAD"), "ref: refs/heads/other\n")
            .expect("rewrite HEAD");
        assert_eq!(cache.branch_for(repo.path()), Some("main".to_owned()));
    }

    #[test]
    fn parse_head_rejects_empty_and_short_input() {
        assert_eq!(parse_head(""), None);
        assert_eq!(parse_head("\n"), None);
        assert_eq!(parse_head("abc"), None); // too short for a hash
        assert_eq!(parse_head("ref: refs/heads/"), None);
    }
}
