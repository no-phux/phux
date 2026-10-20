//! Raw patch text for the diff kit, and the index writes line staging and
//! conflict resolution need. Reads go through the parent's helpers, so
//! `GIT_OPTIONAL_LOCKS=0` and `GIT_TERMINAL_PROMPT=0` apply.

use std::path::{Component, Path, PathBuf};

use super::{MAX_PREVIEW_BYTES, run, run_diff};

/// At most this many untracked files are previewed.
pub const MAX_UNTRACKED: usize = 200;
/// A patch longer than this is read again with three lines of context.
pub const MAX_PATCH_BYTES: usize = 8 * 1024 * 1024;
const FALLBACK_CONTEXT: usize = 3;

/// Plain text, no external tools, and the `a/` and `b/` prefixes
/// `git apply` expects whatever the user's config says.
const DIFF_FLAGS: [&str; 5] = [
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--src-prefix=a/",
    "--dst-prefix=b/",
];

/// Which comparison a patch shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// The working tree against the index, untracked files included.
    Changes,
    /// The index against HEAD, or the empty tree before the first commit.
    Staged,
}

impl Source {
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::Changes => Self::Staged,
            Self::Staged => Self::Changes,
        }
    }
}

/// Unified diff text for the diff kit to parse.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PatchText {
    pub text: String,
    /// Less than was asked for: context fell back to fit
    /// [`MAX_PATCH_BYTES`], or untracked files past [`MAX_UNTRACKED`] were
    /// left out.
    pub reduced: bool,
}

/// Every changed file in `source` with `context` lines of context.
///
/// Unmerged paths are left out (their combined diffs are not unified diffs); the
/// panel shows them from [`read_file`]. [`Source::Changes`] appends a
/// new-file patch per previewable path in `untracked`.
pub fn read_patch(
    root: &Path,
    source: Source,
    untracked: &[String],
    context: usize,
) -> Result<PatchText, String> {
    let patch = collect(root, source, untracked, context)?;
    if patch.text.len() <= MAX_PATCH_BYTES || context <= FALLBACK_CONTEXT {
        return Ok(patch);
    }
    Ok(PatchText {
        reduced: true,
        ..collect(root, source, untracked, FALLBACK_CONTEXT)?
    })
}

/// `patch` split into one section per file, each starting at its `diff ` line.
#[must_use]
pub fn split_files(patch: &str) -> Vec<&str> {
    let mut starts = Vec::new();
    let mut offset = 0;
    for line in patch.split_inclusive('\n') {
        if line.starts_with("diff ") {
            starts.push(offset);
        }
        offset += line.len();
    }
    starts.push(patch.len());
    starts
        .windows(2)
        .map(|pair| &patch[pair[0]..pair[1]])
        .collect()
}

/// A file's text for conflict resolution: UTF-8 and within the preview cap.
pub fn read_file(root: &Path, path: &str) -> Result<String, String> {
    let file = inside(root, path)?;
    let size = std::fs::metadata(&file)
        .map_err(|error| format!("Could not read {path}: {error}"))?
        .len();
    if size > MAX_PREVIEW_BYTES {
        return Err(format!(
            "{path} is {} KiB: too large to open here.",
            size / 1024
        ));
    }
    let bytes = std::fs::read(&file).map_err(|error| format!("Could not read {path}: {error}"))?;
    String::from_utf8(bytes).map_err(|_| format!("{path} is not UTF-8 text."))
}

/// `git apply --cached --recount`, reversed to unstage. Recounting lets a
/// partial patch keep its original hunk headers.
pub(super) fn apply_patch(root: &Path, patch: &str, reverse: bool) -> Result<String, String> {
    let mut args = vec!["apply", "--cached", "--recount", "--whitespace=nowarn"];
    if reverse {
        args.push("-R");
    }
    args.push("-");
    run(root, &args, Some(patch))
}

/// Write the resolved `text` over the still-current conflict and stage it.
pub(super) fn resolve(
    root: &Path,
    path: &str,
    expected: &str,
    text: &str,
) -> Result<String, String> {
    let file = inside(root, path)?;
    if read_file(root, path)? != expected {
        return Err(format!(
            "{path} changed since it was reviewed. Review the refreshed conflict before resolving it."
        ));
    }
    std::fs::write(&file, text).map_err(|error| format!("Could not write {path}: {error}"))?;
    run(root, &["add", "--", path], None)
}

/// `root/path`, refusing a path that is absolute or climbs out of `root`.
fn inside(root: &Path, path: &str) -> Result<PathBuf, String> {
    let relative = Path::new(path);
    let plain = relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)));
    if path.is_empty() || !plain {
        return Err(format!("{path} is not a path inside this checkout."));
    }
    let mut file = root.to_owned();
    for component in relative.components() {
        file.push(component);
        let metadata = std::fs::symlink_metadata(&file)
            .map_err(|error| format!("Could not inspect {path}: {error}"))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "{path} is a symbolic link, not a file to resolve here."
            ));
        }
    }
    Ok(file)
}

fn collect(
    root: &Path,
    source: Source,
    untracked: &[String],
    context: usize,
) -> Result<PatchText, String> {
    let unified = format!("--unified={context}");
    let mut args = vec!["-c", "core.quotePath=false", "diff", unified.as_str()];
    if source == Source::Staged {
        args.push("--cached");
    }
    args.extend(DIFF_FLAGS);
    let mut text = without_unmerged(&run_diff(root, &args)?);
    let untracked = match source {
        Source::Changes => untracked,
        Source::Staged => &[],
    };
    for path in untracked.iter().take(MAX_UNTRACKED) {
        if text.len() > MAX_PATCH_BYTES {
            break;
        }
        text.push_str(&untracked_patch(root, path, &unified)?);
    }
    Ok(PatchText {
        text,
        reduced: untracked.len() > MAX_UNTRACKED,
    })
}

/// `diff` without unmerged paths: their `* Unmerged path` notes and combined
/// (`diff --cc`) sections, which `--diff-filter` does not remove.
fn without_unmerged(diff: &str) -> String {
    split_files(diff)
        .into_iter()
        .filter(|file| file.starts_with("diff --git "))
        .flat_map(|file| file.split_inclusive('\n'))
        .filter(|line| !line.starts_with("* Unmerged path "))
        .collect()
}

/// One untracked file as a new-file patch, or nothing when it is not a
/// previewable text file.
fn untracked_patch(root: &Path, path: &str, unified: &str) -> Result<String, String> {
    let previewable = std::fs::symlink_metadata(root.join(path))
        .is_ok_and(|meta| (meta.is_file() || meta.is_symlink()) && meta.len() <= MAX_PREVIEW_BYTES);
    if !previewable {
        return Ok(String::new());
    }
    let mut args = vec!["-c", "core.quotePath=false", "diff", "--no-index", unified];
    args.extend(DIFF_FLAGS);
    args.extend(["--", "/dev/null", path]);
    let out = run_diff(root, &args)?;
    let binary = out
        .lines()
        .any(|line| line.starts_with("Binary files ") || line == "GIT binary patch");
    Ok(if binary { String::new() } else { out })
}
