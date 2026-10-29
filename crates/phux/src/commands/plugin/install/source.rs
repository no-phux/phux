//! How `phux plugin install REF` chooses a package source.
//!
//! Precedence, first match wins:
//!
//! 1. An existing local directory is copied. An existing local file is a
//!    tarball when its name ends in `.tar`, `.tar.gz`, or `.tgz`.
//! 2. A git URL (`https://…`, `git@…`, `file://…`) is cloned as given.
//!    `--subdir` selects a subdirectory of that clone.
//! 3. `owner/repo` or `owner/repo/sub/dir` that is not an existing path is
//!    GitHub shorthand: clone `https://github.com/owner/repo`, and use any
//!    further segments as the subdirectory.
//!
//! A reference that looks like a path (`./`, `../`, `/`, `~`, a trailing
//! slash, a backslash, or a `.` / `..` segment) is never shorthand. When a
//! reference is both an existing local path and valid shorthand, the local
//! path is chosen and the note says so. `--rev` and `--subdir` apply only
//! to git sources. A shorthand subdirectory and `--subdir` together are
//! refused, and the error names both.

use std::path::{Component, Path, PathBuf};

use super::super::lock::{PluginLockEntry, PluginSourceKind};

/// `https://github.com/<owner>/<repo>` with no extra query or suffix.
const GITHUB_CLONE_HOST: &str = "https://github.com";

/// Schemes `git` is allowed to clone. Anything else, including a `://`
/// that is not at the start, is not a git URL.
const GIT_URL_SCHEMES: &[&str] = &["https://", "ssh://", "git://", "file://"];

/// Where a `phux plugin install` refers its package from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum InstallSource {
    /// Clone `url` with the system `git`, optionally at a branch or tag,
    /// and read the plugin from `subdir` when set.
    Git {
        url: String,
        branch: Option<String>,
        subdir: Option<String>,
    },
    /// Copy a local plugin directory.
    Dir(PathBuf),
    /// Extract a local tarball with the system `tar`.
    Tarball(PathBuf),
}

/// A classified reference, plus a note when a local path won over shorthand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClassifiedSource {
    /// The source install will fetch.
    pub(super) source: InstallSource,
    /// Set when `REF` was also valid GitHub shorthand and an existing local
    /// path was chosen instead.
    pub(super) note: Option<String>,
}

impl InstallSource {
    pub(super) const fn kind(&self) -> PluginSourceKind {
        match self {
            Self::Git { .. } => PluginSourceKind::Git,
            Self::Dir(_) => PluginSourceKind::Dir,
            Self::Tarball(_) => PluginSourceKind::Tarball,
        }
    }

    pub(super) fn source_ref(&self) -> String {
        match self {
            Self::Git { url, .. } => redact_git_url(url),
            Self::Dir(path) | Self::Tarball(path) => path.display().to_string(),
        }
    }

    pub(super) fn branch(&self) -> Option<String> {
        match self {
            Self::Git { branch, .. } => branch.clone(),
            Self::Dir(_) | Self::Tarball(_) => None,
        }
    }

    pub(super) fn subdir(&self) -> Option<&str> {
        match self {
            Self::Git { subdir, .. } => subdir.as_deref(),
            Self::Dir(_) | Self::Tarball(_) => None,
        }
    }
}

/// Classify `reference` against the process working directory.
pub(super) fn classify_source(
    reference: &str,
    rev: Option<&str>,
    subdir: Option<&str>,
) -> Result<ClassifiedSource, String> {
    let base = std::env::current_dir()
        .map_err(|err| format!("could not resolve the working directory: {err}"))?;
    classify_source_in(&base, reference, rev, subdir)
}

/// [`classify_source`] with an explicit base for relative paths.
pub(super) fn classify_source_in(
    base: &Path,
    reference: &str,
    rev: Option<&str>,
    subdir: Option<&str>,
) -> Result<ClassifiedSource, String> {
    if reference.trim().is_empty() {
        return Err(
            "install reference is empty. A git URL, an existing local path, \
             or GitHub owner/repo[/subdir] is required"
                .to_owned(),
        );
    }
    reject_option_token("install reference", reference)?;
    if let Some(rev) = rev {
        if rev.trim().is_empty() {
            return Err("--rev must name a branch, tag, or full commit SHA".to_owned());
        }
        reject_option_token("--rev", rev)?;
    }
    if let Some(subdir) = subdir {
        reject_option_token("--subdir", subdir)?;
    }
    if let Some(local) = classify_local(base, reference, rev, subdir)? {
        return Ok(local);
    }
    if is_git_url(reference) {
        return classify_git_url(reference, rev, subdir);
    }
    if looks_like_url(reference) {
        return Err(format!(
            "{reference:?} is not an allowed git URL. Use https://, ssh://, git://, \
             file://, or git@host:path"
        ));
    }
    if is_path_shaped(reference) {
        return Err(path_shaped_miss(reference));
    }
    if is_tarball_name(reference) {
        return Err(format!(
            "{reference:?} is neither an existing local path nor a git URL. \
             It was chosen as a tarball path because its name ends in .tar, \
             .tar.gz, or .tgz, and that file does not exist"
        ));
    }
    if let Some(shorthand) = parse_github_shorthand(reference) {
        return classify_shorthand(shorthand, rev, subdir);
    }
    Err(format!(
        "{reference:?} is neither an existing local path nor a git URL. \
         GitHub shorthand is owner/repo or owner/repo/sub/dir, which clones \
         {GITHUB_CLONE_HOST}/owner/repo; this reference does not match"
    ))
}

/// Rebuild a source from a lockfile entry. `subdir` is optional so locks
/// written before subdirectory installs still load.
pub(super) fn source_from_lock(entry: &PluginLockEntry) -> Result<InstallSource, String> {
    match entry.source {
        PluginSourceKind::Git => Ok(InstallSource::Git {
            url: entry.source_ref.clone(),
            branch: entry.branch.clone(),
            subdir: entry.subdir.clone(),
        }),
        PluginSourceKind::Dir => {
            let path = PathBuf::from(&entry.source_ref);
            if path.is_dir() {
                Ok(InstallSource::Dir(path))
            } else {
                Err(format!(
                    "recorded source directory {} for plugin {} no longer exists",
                    path.display(),
                    entry.id,
                ))
            }
        }
        PluginSourceKind::Tarball => {
            let path = PathBuf::from(&entry.source_ref);
            if path.is_file() {
                Ok(InstallSource::Tarball(path))
            } else {
                Err(format!(
                    "recorded source tarball {} for plugin {} no longer exists",
                    path.display(),
                    entry.id,
                ))
            }
        }
    }
}

fn classify_local(
    base: &Path,
    reference: &str,
    rev: Option<&str>,
    subdir: Option<&str>,
) -> Result<Option<ClassifiedSource>, String> {
    let path = resolve_ref_path(base, reference);
    if path.is_dir() {
        refuse_git_only_flags(reference, rev, subdir, "directory")?;
        return Ok(Some(ClassifiedSource {
            source: InstallSource::Dir(canonicalize(&path)?),
            note: local_win_note(reference, "directory"),
        }));
    }
    if path.is_file() {
        refuse_git_only_flags(reference, rev, subdir, "file")?;
        if !is_tarball_name(reference) {
            return Err(not_a_tarball(reference));
        }
        return Ok(Some(ClassifiedSource {
            source: InstallSource::Tarball(canonicalize(&path)?),
            note: local_win_note(reference, "tarball"),
        }));
    }
    Ok(None)
}

fn classify_git_url(
    reference: &str,
    rev: Option<&str>,
    subdir: Option<&str>,
) -> Result<ClassifiedSource, String> {
    Ok(ClassifiedSource {
        source: InstallSource::Git {
            url: reference.to_owned(),
            branch: rev.map(str::to_owned),
            subdir: normalize_subdir_flag(subdir)?,
        },
        note: None,
    })
}

fn classify_shorthand(
    shorthand: GitHubShorthand,
    rev: Option<&str>,
    subdir_flag: Option<&str>,
) -> Result<ClassifiedSource, String> {
    let subdir = match (shorthand.subdir, normalize_subdir_flag(subdir_flag)?) {
        (Some(from_ref), Some(from_flag)) => {
            return Err(format!(
                "the reference names subdirectory {from_ref:?} and --subdir names \
                 {from_flag:?}; neither was chosen, pass only one"
            ));
        }
        (Some(subdir), None) | (None, Some(subdir)) => Some(subdir),
        (None, None) => None,
    };
    Ok(ClassifiedSource {
        source: InstallSource::Git {
            url: shorthand.url,
            branch: rev.map(str::to_owned),
            subdir,
        },
        note: None,
    })
}

fn refuse_git_only_flags(
    reference: &str,
    rev: Option<&str>,
    subdir: Option<&str>,
    kind: &str,
) -> Result<(), String> {
    if rev.is_some() {
        return Err(git_only_flag_error("rev", reference, kind));
    }
    if subdir.is_some() {
        return Err(git_only_flag_error("subdir", reference, kind));
    }
    Ok(())
}

fn git_only_flag_error(flag: &str, reference: &str, kind: &str) -> String {
    let choice =
        local_win_note(reference, kind).map_or_else(String::new, |note| format!("{note}. "));
    format!("{choice}--{flag} only applies to git sources")
}

fn local_win_note(reference: &str, kind: &str) -> Option<String> {
    parse_github_shorthand(reference).map(|shorthand| {
        let subdir = shorthand
            .subdir
            .map_or_else(String::new, |subdir| format!(" (subdirectory {subdir})"));
        format!(
            "{reference:?} was chosen as a local {kind} because that path exists; \
             GitHub shorthand {}{subdir} was not used",
            shorthand.url
        )
    })
}

fn not_a_tarball(reference: &str) -> String {
    let mut message =
        format!("{reference} is a file but not a recognized tarball (.tar, .tar.gz, .tgz)");
    if let Some(note) = local_win_note(reference, "file") {
        message.push_str(". ");
        message.push_str(&note);
    }
    message
}

fn path_shaped_miss(reference: &str) -> String {
    format!(
        "{reference:?} is neither an existing local path nor a git URL. \
         It was chosen as a local path, not GitHub shorthand, because a leading \
         ./, ../, ~, or /, a trailing slash, a backslash, or a . or .. segment \
         is never shorthand, and that path does not exist"
    )
}

fn resolve_ref_path(base: &Path, reference: &str) -> PathBuf {
    let path = Path::new(reference);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn canonicalize(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|err| format!("could not resolve {}: {err}", path.display()))
}

fn reject_option_token(label: &str, value: &str) -> Result<(), String> {
    if value.starts_with('-') {
        Err(format!(
            "{label} {value:?} starts with '-'; rejected before git so it cannot be passed as an option"
        ))
    } else {
        Ok(())
    }
}

/// `https://`, `ssh://`, `git://`, `file://` at the start, or `git@host:path`.
/// A `://` anywhere else is not a URL: that is how `--upload-pack=...://x`
/// used to reach `git` as an option.
fn is_git_url(reference: &str) -> bool {
    if reference.starts_with('-') || reference.chars().any(char::is_whitespace) {
        return false;
    }
    if GIT_URL_SCHEMES
        .iter()
        .any(|scheme| reference.starts_with(scheme))
    {
        return true;
    }
    let Some(rest) = reference.strip_prefix("git@") else {
        return false;
    };
    let Some((host, path)) = rest.split_once(':') else {
        return false;
    };
    !host.is_empty() && !host.contains('/') && !path.is_empty()
}

fn looks_like_url(reference: &str) -> bool {
    reference.contains("://") || reference.starts_with("git@")
}

/// Drop `user:pass@` and https token userinfo. `ssh://git@host` stays, because
/// that user is the ssh login, not a secret. The clone still uses the raw URL.
pub(super) fn redact_git_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_owned();
    };
    let scheme = &url[..scheme_end];
    let rest = &url[scheme_end + 3..];
    let Some(at) = rest.find('@') else {
        return url.to_owned();
    };
    let userinfo = &rest[..at];
    let after = &rest[at + 1..];
    if userinfo.contains(':') || scheme == "https" || scheme == "http" {
        format!("{scheme}://{after}")
    } else {
        url.to_owned()
    }
}

/// A reference that must stay a filesystem path even when it is missing.
fn is_path_shaped(reference: &str) -> bool {
    if reference.ends_with(['/', '\\']) || reference.starts_with('~') || reference.contains('\\') {
        return true;
    }
    let path = Path::new(reference);
    path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
}

fn is_tarball_name(name: &str) -> bool {
    // Deliberately exact-case: install refs are user-typed paths, and
    // the managed contract documents the lowercase extensions.
    #[allow(
        clippy::case_sensitive_file_extension_comparisons,
        reason = "documented lowercase tarball extensions"
    )]
    {
        name.ends_with(".tar") || name.ends_with(".tar.gz") || name.ends_with(".tgz")
    }
}

struct GitHubShorthand {
    url: String,
    subdir: Option<String>,
}

/// `owner/repo` or `owner/repo/sub/dir`. A trailing `.git` on the repo
/// segment is stripped. Anything path-shaped, empty, or outside GitHub's
/// owner/repo character set is `None`.
fn parse_github_shorthand(reference: &str) -> Option<GitHubShorthand> {
    if is_path_shaped(reference) || is_git_url(reference) || is_tarball_name(reference) {
        return None;
    }
    let segments: Vec<&str> = reference.split('/').collect();
    if segments.len() < 2 || segments.iter().any(|segment| segment.is_empty()) {
        return None;
    }
    let owner = segments[0];
    let repo = segments[1].strip_suffix(".git").unwrap_or(segments[1]);
    if !valid_owner(owner) || !valid_repo(repo) {
        return None;
    }
    let mut subdir_parts = Vec::new();
    for segment in &segments[2..] {
        if !valid_subdir_segment(segment) {
            return None;
        }
        subdir_parts.push(*segment);
    }
    let subdir = (!subdir_parts.is_empty()).then(|| subdir_parts.join("/"));
    Some(GitHubShorthand {
        url: format!("{GITHUB_CLONE_HOST}/{owner}/{repo}"),
        subdir,
    })
}

fn valid_owner(owner: &str) -> bool {
    let mut chars = owner.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    let mut len = 1_usize;
    let mut prev_hyphen = false;
    for ch in chars {
        len += 1;
        if len > 39 {
            return false;
        }
        if ch.is_ascii_alphanumeric() {
            prev_hyphen = false;
            continue;
        }
        if ch == '-' && !prev_hyphen {
            prev_hyphen = true;
            continue;
        }
        return false;
    }
    !prev_hyphen
}

#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "only the exact lowercase .git suffix is stripped from a GitHub repo name"
)]
fn valid_repo(repo: &str) -> bool {
    let mut chars = repo.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() || repo.ends_with(".git") {
        return false;
    }
    let mut len = 1_usize;
    let mut last_dot = false;
    for ch in chars {
        len += 1;
        if len > 100 {
            return false;
        }
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            last_dot = false;
            continue;
        }
        if ch == '.' {
            last_dot = true;
            continue;
        }
        return false;
    }
    !last_dot
}

fn valid_subdir_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 255
        && !segment.starts_with('.')
        && !segment.ends_with('.')
        && segment
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

/// Normalize `--subdir`. `None` and a missing flag stay unset. A present
/// flag must be a relative path of ordinary segments.
fn normalize_subdir_flag(flag: Option<&str>) -> Result<Option<String>, String> {
    let Some(flag) = flag else {
        return Ok(None);
    };
    let trimmed = flag.trim().trim_end_matches(['/', '\\']);
    if trimmed.is_empty() || is_path_shaped(trimmed) || trimmed.split('/').any(str::is_empty) {
        return Err(format!(
            "--subdir {flag:?} must be a relative subdirectory of the clone, \
             with no ., .., or absolute path"
        ));
    }
    let segments: Vec<&str> = trimmed.split('/').collect();
    if segments
        .iter()
        .any(|segment| !valid_subdir_segment(segment))
    {
        return Err(format!(
            "--subdir {flag:?} must be a relative subdirectory of the clone, \
             with no ., .., or absolute path"
        ));
    }
    Ok(Some(segments.join("/")))
}

#[cfg(test)]
#[path = "classify_tests.rs"]
mod classify_tests;
