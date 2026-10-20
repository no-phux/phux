//! Bounded checkout reads and index writes for a client on another machine.
//!
//! The server's checkout methods call these against the checkout a Terminal
//! is in. Every child runs under a deadline and an output cap, every path is
//! checkout-relative and checked before use, and reads never take the index
//! lock (`GIT_OPTIONAL_LOCKS=0`).

use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::bounded::{Bounded, Run, RunError};
use crate::{Area, failure, parse_status};

mod parse;
mod pr;
mod types;

#[cfg(test)]
use parse::tracking;
use parse::{
    Counts, complete_records, file_text, head_oid, is_binary, line_count, parse_branch,
    parse_commit, text_at_line,
};
pub use pr::{Check, CheckState, PrState, PullRequest, ReviewDecision};
use types::Result;
pub use types::{
    BranchList, BranchRef, Change, CheckoutError, Commit, EntryKind, FileDiff, FileText, Listing,
    ListingEntry, LogPage, Snapshot,
};

/// The longest a `git` child may run.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest `gh` may run; it talks to the network.
pub const GH_TIMEOUT: Duration = Duration::from_secs(20);
/// Most text a diff or file read returns.
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
/// Most status entries, branches, or directory entries returned.
pub const MAX_ENTRIES: usize = 5000;
/// Most commits one log page returns.
pub const MAX_LOG_LIMIT: u32 = 100;
/// Most paths one stage or unstage names.
pub const MAX_PATHS: usize = 1024;

const MAX_LISTING_OUTPUT: usize = 8 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 4096;
/// Untracked files counted for line totals, each at most [`MAX_TEXT_BYTES`].
const MAX_COUNTED_UNTRACKED: usize = 200;
/// Git's own binary sniff looks at the first 8000 bytes for a NUL.
const BINARY_SNIFF: usize = 8000;
/// A directory with more entries than this is cut without sorting the rest.
const MAX_SCANNED_ENTRIES: usize = 50_000;

const GIT_ENV: [(&str, &str); 4] = [
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_LITERAL_PATHSPECS", "1"),
    ("LC_ALL", "C"),
];
/// Plain text, no external tools, stable `a/` and `b/` prefixes.
const DIFF_FLAGS: [&str; 5] = [
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--src-prefix=a/",
    "--dst-prefix=b/",
];

/// One checkout, by its working tree's top level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkout {
    root: PathBuf,
}

impl Checkout {
    /// The checkout containing `dir`.
    pub fn open(dir: &Path) -> Result<Self> {
        if !dir.is_dir() {
            return Err(CheckoutError::NotFound(format!(
                "{} is not a directory",
                dir.display()
            )));
        }
        let out = git_in(dir, &["rev-parse", "--show-toplevel"], 4096)?;
        if !out.status.success() {
            return Err(CheckoutError::NotARepository);
        }
        let top = String::from_utf8(out.stdout).map_err(|_| CheckoutError::NotARepository)?;
        let root = PathBuf::from(top.trim_end_matches(['\n', '\r']));
        if root.as_os_str().is_empty() {
            return Err(CheckoutError::NotARepository);
        }
        Ok(Self { root })
    }

    /// The working tree's top level.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Branch, upstream, and every change with line counts.
    pub fn status(&self) -> Result<Snapshot> {
        let out = self.git_ok(
            &[
                "status",
                "--porcelain=v2",
                "--branch",
                "-z",
                "--untracked-files=all",
            ],
            MAX_LISTING_OUTPUT,
        )?;
        let text = complete_records(&out);
        let status = parse_status(self.root.clone(), &text);
        let unstaged = self.numstat(false);
        let staged = self.numstat(true);
        let mut counted_untracked = 0;
        let mut truncated = out.truncated || status.entries.len() > MAX_ENTRIES;
        let mut changes = Vec::with_capacity(status.entries.len().min(MAX_ENTRIES));
        for entry in status.entries.into_iter().take(MAX_ENTRIES) {
            let counts = match entry.area {
                Area::Staged => staged.get(&entry.path).copied(),
                Area::Unstaged => unstaged.get(&entry.path).copied(),
                Area::Untracked if counted_untracked < MAX_COUNTED_UNTRACKED => {
                    counted_untracked += 1;
                    self.count_untracked(&entry.path)
                }
                Area::Untracked | Area::Conflicted => None,
            };
            let (added, deleted, binary) = counts.unwrap_or((None, None, false));
            changes.push(Change {
                area: entry.area,
                path: entry.path,
                from: entry.from,
                code: entry.code,
                added,
                deleted,
                binary,
            });
        }
        truncated |= unstaged.truncated || staged.truncated;
        Ok(Snapshot {
            branch: status.branch.name,
            head: head_oid(&text),
            upstream: status.branch.upstream,
            ahead: status.branch.ahead,
            behind: status.branch.behind,
            changes,
            truncated,
        })
    }

    /// One file's unified diff in `area`; an untracked file shows whole.
    pub fn diff(&self, path: &str, area: Area) -> Result<FileDiff> {
        let path = relative(path)?;
        let out = match area {
            Area::Staged | Area::Unstaged | Area::Conflicted => {
                let mut args = vec!["diff"];
                if area == Area::Staged {
                    args.push("--cached");
                }
                args.extend(DIFF_FLAGS);
                args.extend(["--", path]);
                self.git(&args, MAX_TEXT_BYTES)?
            }
            Area::Untracked => {
                self.resolve(path, Final::AllowSymlink)?;
                let mut args = vec!["diff", "--no-index"];
                args.extend(DIFF_FLAGS);
                args.extend(["--", "/dev/null", path]);
                self.git(&args, MAX_TEXT_BYTES)?
            }
        };
        // `--no-index` exits 1 when the files differ.
        if !matches!(out.status.code(), Some(0 | 1)) {
            return Err(CheckoutError::Git(failure(&out.stderr, "")));
        }
        let (text, truncated) = text_at_line(&out.stdout, out.truncated);
        let binary = text
            .lines()
            .any(|line| line.starts_with("Binary files ") || line == "GIT binary patch");
        Ok(FileDiff {
            text,
            truncated,
            binary,
        })
    }

    /// `limit` commits after the newest `skip`, newest first; only those
    /// touching `path` when given.
    pub fn log(&self, skip: u32, limit: u32, path: Option<&str>) -> Result<LogPage> {
        let path = path.map(relative).transpose()?;
        if !self.has_head()? {
            return Ok(LogPage::default());
        }
        let limit = limit.clamp(1, MAX_LOG_LIMIT);
        let skip = format!("--skip={skip}");
        let count = format!("--max-count={}", limit + 1);
        let mut args = vec![
            "-c",
            "log.showSignature=false",
            "log",
            "--no-color",
            "-z",
            "--format=%H%x1f%h%x1f%an%x1f%ae%x1f%at%x1f%D%x1f%s",
            &skip,
            &count,
            "HEAD",
            "--",
        ];
        args.extend(path);
        let out = self.git_ok(&args, MAX_LISTING_OUTPUT)?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut commits: Vec<Commit> = text
            .split('\0')
            .filter_map(|record| parse_commit(record.trim_start_matches('\n')))
            .collect();
        let has_more = commits.len() > limit as usize;
        commits.truncate(limit as usize);
        Ok(LogPage { commits, has_more })
    }

    /// Local and remote-tracking branches with their upstream standing.
    pub fn branches(&self) -> Result<BranchList> {
        let head = self.git(&["symbolic-ref", "-q", "--short", "HEAD"], 4096)?;
        let current = head
            .status
            .success()
            .then(|| String::from_utf8_lossy(&head.stdout).trim().to_owned())
            .filter(|name| !name.is_empty());
        let out = self.git_ok(
            &[
                "for-each-ref",
                "--sort=-committerdate",
                "--format=%(refname)%1f%(refname:short)%1f%(upstream:short)%1f%(upstream:track,nobracket)%1f%(objectname:short)%1f%(committerdate:unix)%1f%(contents:subject)",
                "refs/heads",
                "refs/remotes",
            ],
            MAX_LISTING_OUTPUT,
        )?;
        let text = String::from_utf8_lossy(&out.stdout);
        let (mut local, remote): (Vec<_>, Vec<_>) = text
            .lines()
            .filter_map(|line| parse_branch(line, current.as_deref()))
            .partition(|branch| !branch.remote);
        local.extend(remote);
        let truncated = out.truncated || local.len() > MAX_ENTRIES;
        local.truncate(MAX_ENTRIES);
        Ok(BranchList {
            current,
            branches: local,
            truncated,
        })
    }

    /// The entries of `dir` (empty for the top level), tracked and untracked
    /// alike, without `.git`.
    pub fn tree(&self, dir: &str) -> Result<Listing> {
        let (path, prefix) = if dir.is_empty() {
            (self.root.clone(), String::new())
        } else {
            let dir = relative(dir)?;
            (self.resolve(dir, Final::NoSymlink)?, format!("{dir}/"))
        };
        let read = std::fs::read_dir(&path)
            .map_err(|error| io_error(&format!("Could not list {dir}"), &error))?;
        let mut entries = Vec::new();
        let mut truncated = false;
        for entry in read.flatten() {
            if entries.len() >= MAX_SCANNED_ENTRIES {
                truncated = true;
                break;
            }
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if name.eq_ignore_ascii_case(".git") {
                continue;
            }
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            let kind = if meta.file_type().is_symlink() {
                EntryKind::Symlink
            } else if meta.is_dir() {
                EntryKind::Directory
            } else {
                EntryKind::File
            };
            entries.push(ListingEntry {
                size: (kind == EntryKind::File).then_some(meta.len()),
                name,
                kind,
                ignored: false,
            });
        }
        entries.sort_by(|a, b| {
            (a.kind != EntryKind::Directory, &a.name)
                .cmp(&(b.kind != EntryKind::Directory, &b.name))
        });
        truncated |= entries.len() > MAX_ENTRIES;
        entries.truncate(MAX_ENTRIES);
        self.mark_ignored(&prefix, &mut entries);
        Ok(Listing { entries, truncated })
    }

    /// Up to `max_bytes` (at most [`MAX_TEXT_BYTES`]; zero means that cap) of
    /// a regular file as UTF-8 text.
    pub fn read(&self, path: &str, max_bytes: u32) -> Result<FileText> {
        let rel = relative(path)?;
        let file = self.resolve(rel, Final::NoSymlink)?;
        let cap = match usize::try_from(max_bytes) {
            Ok(0) | Err(_) => MAX_TEXT_BYTES,
            Ok(bytes) => bytes.min(MAX_TEXT_BYTES),
        };
        let opened = open_no_follow(&file)
            .map_err(|error| io_error(&format!("Could not read {path}"), &error))?;
        let meta = opened
            .metadata()
            .map_err(|error| io_error(&format!("Could not read {path}"), &error))?;
        if !meta.is_file() {
            return Err(CheckoutError::NotFound(format!("{path} is not a file")));
        }
        let mut bytes = Vec::with_capacity(cap.min(64 * 1024));
        opened
            .take(u64::try_from(cap).unwrap_or(u64::MAX))
            .read_to_end(&mut bytes)
            .map_err(|error| io_error(&format!("Could not read {path}"), &error))?;
        let truncated = meta.len() > bytes.len() as u64;
        Ok(file_text(meta.len(), bytes, truncated))
    }

    /// The current branch's pull request and its checks, from `gh`.
    pub fn pull_request(&self) -> Result<PullRequest> {
        pr::view(&self.root)
    }

    /// Add `paths` to the index.
    pub fn stage(&self, paths: &[String]) -> Result<()> {
        let paths = path_list(paths)?;
        let mut args = vec!["add", "--"];
        args.extend(paths);
        self.git_ok(&args, 64 * 1024).map(drop)
    }

    /// Return `paths` in the index to `HEAD`; before the first commit, take
    /// them out of the index.
    pub fn unstage(&self, paths: &[String]) -> Result<()> {
        let paths = path_list(paths)?;
        let mut args = if self.has_head()? {
            vec!["reset", "-q", "--"]
        } else {
            vec!["rm", "--cached", "-q", "-r", "--ignore-unmatch", "--"]
        };
        args.extend(paths);
        self.git_ok(&args, 64 * 1024).map(drop)
    }

    fn has_head(&self) -> Result<bool> {
        let out = self.git(&["rev-parse", "-q", "--verify", "HEAD^{commit}"], 4096)?;
        Ok(out.status.success())
    }

    /// `(added, deleted, binary)` per destination path.
    fn numstat(&self, cached: bool) -> Counts {
        let mut args = vec!["diff", "--numstat", "-z", "--no-ext-diff", "--no-textconv"];
        if cached {
            args.push("--cached");
        }
        match self.git(&args, MAX_LISTING_OUTPUT) {
            Ok(out) if out.status.success() => Counts::parse(&out.stdout, out.truncated),
            _ => Counts::default(),
        }
    }

    /// Line counts for a small untracked text file.
    fn count_untracked(&self, path: &str) -> Option<(Option<u32>, Option<u32>, bool)> {
        let file = self.root.join(path);
        let meta = file.symlink_metadata().ok()?;
        if !meta.is_file() || meta.len() > MAX_TEXT_BYTES as u64 {
            return None;
        }
        let bytes = std::fs::read(&file).ok()?;
        if is_binary(&bytes) {
            return Some((None, None, true));
        }
        Some((Some(line_count(&bytes)), Some(0), false))
    }

    fn mark_ignored(&self, prefix: &str, entries: &mut [ListingEntry]) {
        if entries.is_empty() {
            return;
        }
        let mut stdin = Vec::new();
        for entry in entries.iter() {
            stdin.extend_from_slice(prefix.as_bytes());
            stdin.extend_from_slice(entry.name.as_bytes());
            stdin.push(0);
        }
        // `check-ignore` refuses literal pathspecs. An ignore mark is a hint:
        // a failure leaves every entry unmarked rather than failing the
        // listing.
        let Ok(out) = run_git(
            &self.root,
            &["--no-literal-pathspecs", "check-ignore", "-z", "--stdin"],
            MAX_LISTING_OUTPUT,
            Some(stdin),
        ) else {
            return;
        };
        // Exit 1: nothing is ignored.
        if !matches!(out.status.code(), Some(0 | 1)) {
            return;
        }
        let ignored: std::collections::HashSet<&[u8]> = out
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .collect();
        for entry in entries {
            let full = format!("{prefix}{}", entry.name);
            entry.ignored = ignored.contains(full.as_bytes());
        }
    }

    /// `root/rel` with every component checked: no symlink before the last,
    /// and the last per `last`.
    fn resolve(&self, rel: &str, last: Final) -> Result<PathBuf> {
        let mut path = self.root.clone();
        let components: Vec<_> = Path::new(rel).components().collect();
        for (index, component) in components.iter().enumerate() {
            path.push(component);
            let meta = path
                .symlink_metadata()
                .map_err(|_| CheckoutError::NotFound(format!("{rel} does not exist")))?;
            let is_last = index + 1 == components.len();
            if meta.file_type().is_symlink() && !(is_last && last == Final::AllowSymlink) {
                return Err(CheckoutError::InvalidPath(format!(
                    "{rel} crosses a symbolic link"
                )));
            }
        }
        Ok(path)
    }

    fn git(&self, args: &[&str], max_stdout: usize) -> Result<Bounded> {
        run_git(&self.root, args, max_stdout, None)
    }

    /// Run git and require success.
    fn git_ok(&self, args: &[&str], max_stdout: usize) -> Result<Bounded> {
        let out = self.git(args, max_stdout)?;
        if out.status.success() || out.truncated {
            Ok(out)
        } else {
            Err(CheckoutError::Git(failure(
                &out.stderr,
                &String::from_utf8_lossy(&out.stdout),
            )))
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Final {
    AllowSymlink,
    NoSymlink,
}

fn git_in(dir: &Path, args: &[&str], max_stdout: usize) -> Result<Bounded> {
    run_git(dir, args, max_stdout, None)
}

fn run_git(
    dir: &Path,
    args: &[&str],
    max_stdout: usize,
    stdin: Option<Vec<u8>>,
) -> Result<Bounded> {
    let mut full = vec!["-c", "core.quotePath=false", "-c", "color.ui=false"];
    full.extend_from_slice(args);
    Run {
        program: "git",
        dir,
        args: &full,
        env: &GIT_ENV,
        timeout: GIT_TIMEOUT,
        max_stdout,
        stdin,
    }
    .exec()
    .map_err(|error| match error {
        RunError::Timeout => CheckoutError::Timeout,
        RunError::Spawn(error) => CheckoutError::Git(format!("Could not run git: {error}")),
        RunError::Wait(message) => CheckoutError::Git(format!("git did not finish: {message}")),
    })
}

/// A checkout-relative path: non-empty, plain components only, nothing
/// named `.git`.
pub fn relative(path: &str) -> Result<&str> {
    let invalid = |why: &str| CheckoutError::InvalidPath(format!("{path:?} {why}"));
    if path.is_empty() || path.len() > MAX_PATH_BYTES || path.contains('\0') {
        return Err(invalid("is not a path inside this checkout"));
    }
    for component in Path::new(path).components() {
        match component {
            Component::Normal(name) if name.eq_ignore_ascii_case(".git") => {
                return Err(invalid("is inside .git"));
            }
            Component::Normal(_) => {}
            _ => return Err(invalid("is not a plain path inside this checkout")),
        }
    }
    Ok(path)
}

fn path_list(paths: &[String]) -> Result<Vec<&str>> {
    if paths.is_empty() || paths.len() > MAX_PATHS {
        return Err(CheckoutError::InvalidRequest(format!(
            "name between 1 and {MAX_PATHS} paths"
        )));
    }
    paths.iter().map(|path| relative(path)).collect()
}

fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

fn io_error(what: &str, error: &std::io::Error) -> CheckoutError {
    if error.kind() == std::io::ErrorKind::NotFound {
        CheckoutError::NotFound(format!("{what}: {error}"))
    } else {
        CheckoutError::Io(format!("{what}: {error}"))
    }
}

#[cfg(test)]
mod tests;
