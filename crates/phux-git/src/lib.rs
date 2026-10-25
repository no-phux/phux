//! Native Git over the user's own `git`: a checkout's branch and changed
//! files, raw patches, and the index and commit writes. The desktop Changes
//! panel uses it on local folders; the server's checkout methods use the
//! bounded [`checkout`] surface on the checkout a Terminal is in.
//!
//! Everything runs the user's own `git` against a local directory, so hooks,
//! config, credentials and ignore rules behave exactly as they do in the
//! terminal. Reads pass `GIT_OPTIONAL_LOCKS=0`: a status poll never takes the
//! index lock an agent's `git` in a pane may need. Calls block; callers run
//! them off their UI thread or async runtime.

#![allow(
    clippy::redundant_pub_crate,
    reason = "crate-private helpers stay `pub(super)`; `unreachable_pub` rejects plain `pub`"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[cfg(unix)]
mod bounded;
#[cfg(unix)]
pub mod checkout;
mod patch;
mod stage;

pub use patch::{Selection, Side, line_staging_refusal, partial_patch};
pub use stage::{
    MAX_PATCH_BYTES, MAX_UNTRACKED, PatchText, Source, read_file, read_patch, split_files,
};

/// An untracked file larger than this is not read for its preview.
const MAX_PREVIEW_BYTES: u64 = 1024 * 1024;

/// Where a change sits relative to the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Area {
    /// Unmerged: both sides changed it.
    Conflicted,
    /// In the index, ready to commit.
    Staged,
    /// Tracked and changed in the working tree.
    Unstaged,
    /// Not tracked yet.
    Untracked,
}

/// One changed path in one [`Area`]. A file staged and then edited again is
/// two entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub area: Area,
    /// Repository-relative path.
    pub path: String,
    /// The path a rename or copy came from.
    pub from: Option<String>,
    /// Porcelain status letter: `M`, `A`, `D`, `R`, `C`, `T`, `U` or `?`.
    pub code: char,
}

impl Entry {
    /// The file name, then its directory, for a two-tone row.
    #[must_use]
    pub fn split_name(&self) -> (&str, &str) {
        match self.path.rsplit_once('/') {
            Some((dir, name)) => (name, dir),
            None => (self.path.as_str(), ""),
        }
    }
}

/// The checked-out branch and how it stands against its upstream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Branch {
    /// `None` when HEAD is detached.
    pub name: Option<String>,
    /// Abbreviated HEAD commit; `None` before the first commit.
    pub commit: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

/// One checkout's state as of one `git status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    /// The working tree's top level.
    pub root: PathBuf,
    pub branch: Branch,
    /// Sorted by area, then path.
    pub entries: Vec<Entry>,
}

impl Status {
    /// Entries in `area`, in path order.
    pub fn in_area(&self, area: Area) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(move |entry| entry.area == area)
    }

    #[must_use]
    pub fn count(&self, area: Area) -> usize {
        self.in_area(area).count()
    }

    /// The repository's folder name.
    #[must_use]
    pub fn name(&self) -> String {
        self.root.file_name().map_or_else(
            || self.root.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }
}

/// A change to the index or history the panel can make.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Add these paths to the index.
    Stage(Vec<String>),
    /// Return these paths in the index to HEAD.
    Unstage(Vec<String>),
    StageAll,
    UnstageAll,
    /// Commit the index with this message.
    Commit(String),
    /// Apply a [`partial_patch`] to the index, reversed to unstage.
    Apply {
        patch: String,
        reverse: bool,
    },
    /// Write a conflicted file's resolved text, then stage it.
    Resolve {
        path: String,
        /// The conflict text the reviewer actually saw; stale resolutions refuse.
        expected: String,
        text: String,
    },
}

/// The status of the checkout containing `dir`. `Ok(None)` means `dir` is not
/// in a Git working tree.
pub fn read_status(dir: &Path) -> Result<Option<Status>, String> {
    if !dir.is_dir() {
        return Err(format!("{} is not a folder on this Mac.", dir.display()));
    }
    let Ok(top) = run(dir, &["rev-parse", "--show-toplevel"], None) else {
        return Ok(None);
    };
    let root = PathBuf::from(top.trim_end_matches(['\n', '\r']));
    let out = run(
        &root,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
        ],
        None,
    )?;
    Ok(Some(parse_status(root, &out)))
}

/// Apply `op` to the checkout at `root`.
pub fn apply(root: &Path, op: &Op) -> Result<(), String> {
    match op {
        Op::Stage(paths) => run(root, &with_paths(&["add", "--"], paths), None),
        Op::Unstage(paths) => run(root, &with_paths(&["reset", "-q", "--"], paths), None),
        Op::StageAll => run(root, &["add", "-A"], None),
        Op::UnstageAll => run(root, &["reset", "-q"], None),
        Op::Commit(message) => run(root, &["commit", "-q", "-F", "-"], Some(message)),
        Op::Apply { patch, reverse } => stage::apply_patch(root, patch, *reverse),
        Op::Resolve {
            path,
            expected,
            text,
        } => stage::resolve(root, path, expected, text),
    }
    .map(drop)
}

fn with_paths<'a>(head: &[&'a str], paths: &'a [String]) -> Vec<&'a str> {
    let mut args = head.to_vec();
    args.extend(paths.iter().map(String::as_str));
    args
}

fn command(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Run git; a non-zero exit is an error carrying git's own message.
fn run(dir: &Path, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
    let output = exec(dir, args, stdin)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(failure(
            &String::from_utf8_lossy(&output.stderr),
            &String::from_utf8_lossy(&output.stdout),
        ))
    }
}

/// `git diff` exits 1 when `--no-index` finds a difference.
fn run_diff(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = exec(dir, args, None)?;
    match output.status.code() {
        Some(0 | 1) => String::from_utf8(output.stdout).map_err(|_| {
            "This diff contains non-UTF-8 data. Review and stage it in the terminal instead."
                .to_owned()
        }),
        _ => Err(failure(
            &String::from_utf8_lossy(&output.stderr),
            &String::from_utf8_lossy(&output.stdout),
        )),
    }
}

fn exec(dir: &Path, args: &[&str], stdin: Option<&str>) -> Result<Output, String> {
    use std::io::Write as _;
    let mut command = command(dir, args);
    command.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not run git: {error}"))?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(text.as_bytes())
            .map_err(|error| format!("Could not write to git: {error}"))?;
    }
    child
        .wait_with_output()
        .map_err(|error| format!("git did not finish: {error}"))
}

/// Git's message, trimmed to what fits in a notice.
fn failure(stderr: &str, stdout: &str) -> String {
    let text = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    let lines: Vec<&str> = text.trim().lines().collect();
    let tail = &lines[lines.len().saturating_sub(12)..];
    if tail.is_empty() {
        "git failed without a message.".into()
    } else {
        tail.join("\n")
    }
}

/// Parse `git status --porcelain=v2 --branch -z`.
#[must_use]
pub fn parse_status(root: PathBuf, out: &str) -> Status {
    let mut branch = Branch::default();
    let mut entries = Vec::new();
    let mut records = out.split('\0');
    while let Some(record) = records.next() {
        if let Some(header) = record.strip_prefix("# ") {
            parse_header(header, &mut branch);
            continue;
        }
        let Some((kind, rest)) = record.split_once(' ') else {
            continue;
        };
        match kind {
            "1" => tracked(rest, 7, None, &mut entries),
            // A rename's source is the next record; output cut before it
            // leaves a rename that cannot be shown truthfully, so drop it.
            "2" => {
                let Some(from) = records.next().filter(|from| !from.is_empty()) else {
                    continue;
                };
                tracked(rest, 8, Some(from.to_owned()), &mut entries);
            }
            "u" => {
                if let Some(path) = rest.splitn(10, ' ').nth(9) {
                    entries.push(Entry {
                        area: Area::Conflicted,
                        path: path.to_owned(),
                        from: None,
                        code: 'U',
                    });
                }
            }
            "?" => entries.push(Entry {
                area: Area::Untracked,
                path: rest.to_owned(),
                from: None,
                code: '?',
            }),
            _ => {}
        }
    }
    entries.sort_by(|a, b| (a.area, &a.path).cmp(&(b.area, &b.path)));
    Status {
        root,
        branch,
        entries,
    }
}

fn parse_header(header: &str, branch: &mut Branch) {
    let Some((key, value)) = header.split_once(' ') else {
        return;
    };
    match key {
        "branch.oid" if value != "(initial)" => {
            branch.commit = Some(value.chars().take(8).collect());
        }
        "branch.head" if value != "(detached)" => branch.name = Some(value.to_owned()),
        "branch.upstream" => branch.upstream = Some(value.to_owned()),
        "branch.ab" => {
            for part in value.split(' ') {
                if let Some(ahead) = part.strip_prefix('+') {
                    branch.ahead = ahead.parse().unwrap_or(0);
                } else if let Some(behind) = part.strip_prefix('-') {
                    branch.behind = behind.parse().unwrap_or(0);
                }
            }
        }
        _ => {}
    }
}

/// An ordinary (`1`) or renamed (`2`) record: `XY` then `skip` fields, then
/// the path, which may itself contain spaces.
fn tracked(rest: &str, skip: usize, from: Option<String>, entries: &mut Vec<Entry>) {
    let mut fields = rest.splitn(skip + 1, ' ');
    let Some(xy) = fields.next() else {
        return;
    };
    let Some(path) = fields.nth(skip - 1) else {
        return;
    };
    let mut codes = xy.chars();
    let (index, tree) = (codes.next().unwrap_or('.'), codes.next().unwrap_or('.'));
    if index != '.' {
        entries.push(Entry {
            area: Area::Staged,
            path: path.to_owned(),
            from,
            code: index,
        });
    }
    if tree != '.' {
        entries.push(Entry {
            area: Area::Unstaged,
            path: path.to_owned(),
            from: None,
            code: tree,
        });
    }
}

#[cfg(test)]
mod patch_tests;
#[cfg(test)]
mod stage_tests;
#[cfg(test)]
mod tests;
