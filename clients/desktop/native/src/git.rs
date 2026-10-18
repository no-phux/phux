//! Native Git for the Changes panel: a checkout's branch, changed files and
//! diffs, plus stage, unstage and commit.
//!
//! Everything runs the user's own `git` against a local directory, so hooks,
//! config, credentials and ignore rules behave exactly as they do in the
//! terminal. Reads pass `GIT_OPTIONAL_LOCKS=0`: a status poll never takes the
//! index lock an agent's `git` in a pane may need. Calls block; the desk runs
//! them off the UI thread.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A diff longer than this is cut, with a note saying so.
pub const MAX_DIFF_LINES: usize = 4000;
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

/// What one diff line is, for its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// `diff --git`, `index`, `---`, `+++` and mode lines.
    Meta,
    /// `@@ -a,b +c,d @@`.
    Hunk,
    Added,
    Removed,
    Context,
    /// `\ No newline at end of file`, binary notices, truncation.
    Note,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
    /// The line's number in the new file, or in the old one when removed.
    pub number: Option<u32>,
}

/// A rendered diff for one entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub lines: Vec<DiffLine>,
    pub added: usize,
    pub removed: usize,
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

/// The diff `entry` shows: the index against HEAD when staged, the working
/// tree against the index otherwise, and the whole file when untracked.
pub fn read_diff(root: &Path, entry: &Entry) -> Result<Diff, String> {
    let path = entry.path.as_str();
    let out = match entry.area {
        Area::Staged => run_diff(
            root,
            &[
                "diff",
                "--cached",
                "--no-color",
                "--no-ext-diff",
                "--",
                path,
            ],
        )?,
        Area::Unstaged | Area::Conflicted => {
            run_diff(root, &["diff", "--no-color", "--no-ext-diff", "--", path])?
        }
        Area::Untracked => {
            let size = std::fs::metadata(root.join(path)).map_or(0, |meta| meta.len());
            if size > MAX_PREVIEW_BYTES {
                return Ok(note(&format!(
                    "New file, {} KiB: too large to preview.",
                    size / 1024
                )));
            }
            run_diff(
                root,
                &[
                    "diff",
                    "--no-index",
                    "--no-color",
                    "--no-ext-diff",
                    "--",
                    "/dev/null",
                    path,
                ],
            )?
        }
    };
    if out.trim().is_empty() {
        return Ok(note("No textual changes."));
    }
    Ok(parse_diff(&out))
}

/// Apply `op` to the checkout at `root`.
pub fn apply(root: &Path, op: &Op) -> Result<(), String> {
    match op {
        Op::Stage(paths) => run(root, &with_paths(&["add", "--"], paths), None),
        Op::Unstage(paths) => run(root, &with_paths(&["reset", "-q", "--"], paths), None),
        Op::StageAll => run(root, &["add", "-A"], None),
        Op::UnstageAll => run(root, &["reset", "-q"], None),
        Op::Commit(message) => run(root, &["commit", "-q", "-F", "-"], Some(message)),
    }
    .map(drop)
}

fn with_paths<'a>(head: &[&'a str], paths: &'a [String]) -> Vec<&'a str> {
    let mut args = head.to_vec();
    args.extend(paths.iter().map(String::as_str));
    args
}

fn note(text: &str) -> Diff {
    Diff {
        lines: vec![DiffLine {
            kind: LineKind::Note,
            text: text.to_owned(),
            number: None,
        }],
        ..Diff::default()
    }
}

fn command(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Run git; a non-zero exit is an error carrying git's own message.
fn run(dir: &Path, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
    let (code, stdout, stderr) = exec(dir, args, stdin)?;
    if code == Some(0) {
        Ok(stdout)
    } else {
        Err(failure(&stderr, &stdout))
    }
}

/// `git diff` exits 1 when `--no-index` finds a difference.
fn run_diff(dir: &Path, args: &[&str]) -> Result<String, String> {
    let (code, stdout, stderr) = exec(dir, args, None)?;
    match code {
        Some(0 | 1) => Ok(stdout),
        _ => Err(failure(&stderr, &stdout)),
    }
}

fn exec(
    dir: &Path,
    args: &[&str],
    stdin: Option<&str>,
) -> Result<(Option<i32>, String, String), String> {
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
    let output = child
        .wait_with_output()
        .map_err(|error| format!("git did not finish: {error}"))?;
    Ok((
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
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
            // A rename's source is the next record.
            "2" => {
                let from = records.next().map(str::to_owned);
                tracked(rest, 8, from, &mut entries);
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
            from: from.clone(),
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

/// Classify a unified diff's lines, cut at [`MAX_DIFF_LINES`].
#[must_use]
pub fn parse_diff(out: &str) -> Diff {
    let mut diff = Diff::default();
    // Next old- and new-side line numbers; `None` outside a hunk.
    let mut at: Option<(u32, u32)> = None;
    for (index, line) in out.lines().enumerate() {
        if index == MAX_DIFF_LINES {
            diff.lines.push(DiffLine {
                kind: LineKind::Note,
                text: format!("Diff cut at {MAX_DIFF_LINES} lines; open the file to see the rest."),
                number: None,
            });
            break;
        }
        let kind = classify(line, at.is_some());
        let number = match (kind, at.as_mut()) {
            (LineKind::Added, Some((_, new))) => Some(step(new)),
            (LineKind::Removed, Some((old, _))) => Some(step(old)),
            (LineKind::Context, Some((old, new))) => {
                step(old);
                Some(step(new))
            }
            _ => None,
        };
        match kind {
            LineKind::Hunk => at = hunk_start(line),
            LineKind::Meta if line.starts_with("diff ") => at = None,
            LineKind::Added => diff.added += 1,
            LineKind::Removed => diff.removed += 1,
            _ => {}
        }
        diff.lines.push(DiffLine {
            kind,
            text: line.replace('\t', "    "),
            number,
        });
    }
    diff
}

fn classify(line: &str, in_hunk: bool) -> LineKind {
    if line.starts_with("diff ") {
        return LineKind::Meta;
    }
    if line.starts_with("@@") {
        return LineKind::Hunk;
    }
    if !in_hunk {
        return if line.starts_with("Binary files") {
            LineKind::Note
        } else {
            LineKind::Meta
        };
    }
    match line.as_bytes().first() {
        Some(b'+') => LineKind::Added,
        Some(b'-') => LineKind::Removed,
        Some(b'\\') => LineKind::Note,
        _ => LineKind::Context,
    }
}

/// Return the current line number and advance it.
fn step(line: &mut u32) -> u32 {
    let current = *line;
    *line += 1;
    current
}

/// The first old and new line numbers of `@@ -a[,b] +c[,d] @@`.
fn hunk_start(header: &str) -> Option<(u32, u32)> {
    let mut sides = header.split(' ').skip(1);
    let side = |field: Option<&str>, sign: char| -> Option<u32> {
        field?.strip_prefix(sign)?.split(',').next()?.parse().ok()
    };
    let old = side(sides.next(), '-')?;
    let new = side(sides.next(), '+')?;
    Some((old, new))
}

#[cfg(test)]
mod tests;
