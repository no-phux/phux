//! Raw patch text for the diff kit, and the index writes line staging and
//! conflict resolution need.
//!
//! Reads follow the parent module's discipline: the same `git` helpers, so
//! `GIT_OPTIONAL_LOCKS=0` and `GIT_TERMINAL_PROMPT=0` apply throughout.

use std::path::{Component, Path, PathBuf};

use super::{MAX_PREVIEW_BYTES, run, run_diff};

/// At most this many untracked files are appended to a
/// [`Source::Changes`] patch.
pub const MAX_UNTRACKED: usize = 200;
/// A patch longer than this is read again with less context, then cut.
pub const MAX_PATCH_BYTES: usize = 8 * 1024 * 1024;
/// The context [`read_patch`] falls back to when a patch is too long.
const FALLBACK_CONTEXT: usize = 3;

/// Which comparison a patch shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// The working tree against the index, plus untracked files as new-file
    /// diffs.
    Changes,
    /// The index against HEAD, or against the empty tree before the first
    /// commit.
    Staged,
}

/// Unified diff text for the diff kit to parse.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PatchText {
    pub text: String,
    /// The text shows less than was asked for: its context was reduced to
    /// fit [`MAX_PATCH_BYTES`], files were cut at that limit, or untracked
    /// files beyond [`MAX_UNTRACKED`] were left out.
    pub reduced: bool,
}

/// Raw unified diff text for every changed file in `source`, with `context`
/// lines of context. Unmerged paths are left out: their combined diffs are
/// not unified diffs, and the panel resolves them from [`read_file`].
///
/// [`Source::Changes`] appends one new-file patch per path in `untracked`
/// (repository-relative), skipping folders, binaries and files over the
/// preview cap. A text over [`MAX_PATCH_BYTES`] is read once more with three
/// lines of context, then cut at a file boundary if still too long.
pub fn read_patch(
    root: &Path,
    source: Source,
    untracked: &[String],
    context: usize,
) -> Result<PatchText, String> {
    let first = collect(root, source, untracked, context)?;
    if first.text.len() <= MAX_PATCH_BYTES || context <= FALLBACK_CONTEXT {
        return Ok(fit(first));
    }
    let mut second = collect(root, source, untracked, FALLBACK_CONTEXT)?;
    second.reduced = true;
    Ok(fit(second))
}

/// A file's text for conflict resolution, refused over the preview cap or
/// when it is not UTF-8.
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

/// `git apply --cached`, reversed for unstaging, with `patch` on stdin.
pub(super) fn apply_patch(root: &Path, patch: &str, reverse: bool) -> Result<String, String> {
    let mut args = vec!["apply", "--cached"];
    if reverse {
        args.push("-R");
    }
    args.extend(["--whitespace=nowarn", "-"]);
    run(root, &args, Some(patch))
}

/// Write the resolved `text` over `path` and stage it.
pub(super) fn resolve(root: &Path, path: &str, text: &str) -> Result<String, String> {
    let file = inside(root, path)?;
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
    Ok(root.join(relative))
}

fn collect(
    root: &Path,
    source: Source,
    untracked: &[String],
    context: usize,
) -> Result<PatchText, String> {
    let unified = format!("--unified={context}");
    let mut args = vec!["-c", "core.quotePath=false", "diff"];
    if source == Source::Staged {
        args.push("--cached");
    }
    args.extend(diff_flags());
    args.push(unified.as_str());
    let mut text = without_unmerged(&run_diff(root, &args)?);
    if source == Source::Staged {
        return Ok(PatchText {
            text,
            reduced: false,
        });
    }
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

/// Flags every patch read shares: plain text, no external tools, and the
/// `a/` and `b/` prefixes `git apply` expects whatever the user's config.
fn diff_flags() -> [&'static str; 5] {
    [
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--src-prefix=a/",
        "--dst-prefix=b/",
    ]
}

/// `diff` without its unmerged paths: their `* Unmerged path` notes and
/// combined (`diff --cc`) sections, which `--diff-filter` does not remove.
fn without_unmerged(diff: &str) -> String {
    let mut combined = false;
    let mut kept = String::with_capacity(diff.len());
    for line in diff.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            combined = false;
        } else if line.starts_with("diff --cc ") || line.starts_with("diff --combined ") {
            combined = true;
        }
        if !combined && !line.starts_with("* Unmerged path ") {
            kept.push_str(line);
        }
    }
    kept
}

/// One untracked file as a new-file patch, or nothing when it is not a
/// previewable text file.
fn untracked_patch(root: &Path, path: &str, unified: &str) -> Result<String, String> {
    let Ok(meta) = std::fs::symlink_metadata(root.join(path)) else {
        return Ok(String::new());
    };
    if !(meta.is_file() || meta.is_symlink()) || meta.len() > MAX_PREVIEW_BYTES {
        return Ok(String::new());
    }
    let mut args = vec!["-c", "core.quotePath=false", "diff", "--no-index"];
    args.extend(diff_flags());
    args.extend([unified, "--", "/dev/null", path]);
    let out = run_diff(root, &args)?;
    let binary = out
        .lines()
        .any(|line| line.starts_with("Binary files ") || line == "GIT binary patch");
    Ok(if binary { String::new() } else { out })
}

/// Cut `patch` at the last file boundary within [`MAX_PATCH_BYTES`].
fn fit(mut patch: PatchText) -> PatchText {
    if patch.text.len() <= MAX_PATCH_BYTES {
        return patch;
    }
    let mut limit = MAX_PATCH_BYTES;
    while !patch.text.is_char_boundary(limit) {
        limit -= 1;
    }
    let keep = patch.text[..limit]
        .rfind("\ndiff --git ")
        .map_or(0, |ix| ix + 1);
    patch.text.truncate(keep);
    patch.reduced = true;
    patch
}
