//! A pull request's patch: what GitHub's "Files changed" shows, the head
//! against its merge base with the base.
//!
//! A local clone gives every line of context, so a fold can open anywhere,
//! and needs no network once the commits are here (an agent that pushed from
//! this machine already has them). Missing commits are fetched without
//! touching any ref, `FETCH_HEAD` included. Without a clone, GitHub's own
//! patch has three lines of context.

use std::path::Path;
use std::time::Duration;

use super::gh::{self, Result as GhResult};
use super::process::{self, Child, Failure};
use crate::{MAX_PATCH_BYTES, PatchText};

const GIT_ENV: [(&str, &str); 3] = [
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GCM_INTERACTIVE", "never"),
];
const READ_TIMEOUT: Duration = Duration::from_secs(20);
const FETCH_TIMEOUT: Duration = Duration::from_secs(90);
/// The context a diff too large for [`MAX_PATCH_BYTES`] falls back to.
const FALLBACK_CONTEXT: usize = 3;

/// What the local diff is between.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Range<'a> {
    pub number: u64,
    pub base_ref: &'a str,
    pub base_oid: &'a str,
    pub head_oid: &'a str,
}

fn git(root: &Path, args: &[&str], timeout: Duration, max: usize) -> Result<Vec<u8>, String> {
    let mut full = vec![
        "-c",
        "core.quotePath=false",
        "-c",
        "color.ui=false",
        "-c",
        "core.fsmonitor=false",
    ];
    full.extend_from_slice(args);
    let out = process::exec(Child {
        program: "git",
        dir: root,
        args: &full,
        env: &GIT_ENV,
        stdin: None,
        timeout,
        max_stdout: max,
    })
    .map_err(|error| match error {
        Failure::Timeout => "git did not finish in time.".to_owned(),
        Failure::NotFound => "Git is not installed.".to_owned(),
        Failure::Spawn(error) => format!("Could not run git: {error}"),
        Failure::Wait(message) => message,
    })?;
    // A capped read is killed; the caller sees the cap reached and decides.
    if out.success || out.truncated {
        Ok(out.stdout)
    } else {
        Err(crate::failure(
            &out.stderr,
            &String::from_utf8_lossy(&out.stdout),
        ))
    }
}

fn has_commit(root: &Path, oid: &str) -> bool {
    let object = format!("{oid}^{{commit}}");
    !oid.is_empty() && git(root, &["cat-file", "-e", &object], READ_TIMEOUT, 0).is_ok()
}

/// The remote in `root` whose URL names `repo` (`owner/name`), `origin` first.
#[must_use]
pub fn remote_for(root: &Path, repo: &str) -> Option<String> {
    let out = git(root, &["remote", "-v"], READ_TIMEOUT, 1024 * 1024).ok()?;
    let out = String::from_utf8_lossy(&out);
    let mut matches: Vec<&str> = out
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let (name, url) = (fields.next()?, fields.next()?);
            repo_of_url(url)
                .is_some_and(|named| named.eq_ignore_ascii_case(repo))
                .then_some(name)
        })
        .collect();
    matches.sort_by_key(|name| *name != "origin");
    matches.first().map(|name| (*name).to_owned())
}

/// `owner/name` of a GitHub remote URL in any of Git's spellings.
#[must_use]
pub fn repo_of_url(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    let path = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?.1,
        // scp-like `git@host:owner/name`
        None => url.split_once(':')?.1,
    };
    let mut parts = path.rsplit('/');
    let name = parts.next()?;
    let owner = parts.next()?;
    (!owner.is_empty() && !name.is_empty()).then(|| format!("{owner}/{name}"))
}

/// Bring the pull request's head and base into `root` when they are not
/// already, writing no ref.
fn ensure_commits(root: &Path, remote: &str, range: &Range<'_>) -> Result<(), String> {
    if has_commit(root, range.head_oid) && has_commit(root, range.base_oid) {
        return Ok(());
    }
    let head = format!("refs/pull/{}/head", range.number);
    let base = format!("refs/heads/{}", range.base_ref);
    let args = [
        "fetch",
        "--quiet",
        "--no-tags",
        "--no-write-fetch-head",
        "--no-recurse-submodules",
        "--refmap=",
        remote,
        &head,
        &base,
    ];
    git(root, &args, FETCH_TIMEOUT, 1024 * 1024)?;
    for oid in [range.head_oid, range.base_oid] {
        if !has_commit(root, oid) {
            return Err(format!(
                "{oid} is not on {remote} yet; showing GitHub's diff."
            ));
        }
    }
    Ok(())
}

/// The patch from a local clone at `root`, fetching from `remote` when needed.
pub fn local(
    root: &Path,
    remote: &str,
    range: &Range<'_>,
    context: usize,
) -> Result<PatchText, String> {
    ensure_commits(root, remote, range)?;
    let base = git(
        root,
        &["merge-base", range.base_oid, range.head_oid],
        READ_TIMEOUT,
        4096,
    )?;
    let base = String::from_utf8_lossy(&base).trim().to_owned();
    let diff = |context: usize| {
        let unified = format!("--unified={context}");
        let args = [
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--find-renames",
            &unified,
            &base,
            range.head_oid,
        ];
        git(root, &args, READ_TIMEOUT, MAX_PATCH_BYTES + 1)
    };
    let mut out = diff(context)?;
    let reduced = out.len() > MAX_PATCH_BYTES && context > FALLBACK_CONTEXT;
    if reduced {
        out = diff(FALLBACK_CONTEXT)?;
    }
    if out.len() > MAX_PATCH_BYTES {
        return Err("This pull request's diff is too large to show here.".into());
    }
    let text = String::from_utf8(out)
        .map_err(|_| "This diff contains non-UTF-8 data; showing GitHub's diff.".to_owned())?;
    Ok(PatchText { text, reduced })
}

/// GitHub's patch for `repo#number`, with GitHub's three lines of context.
pub fn remote(repo: &str, number: u64) -> GhResult<PatchText> {
    let number = number.to_string();
    let out = gh::run(
        &std::env::temp_dir(),
        &["pr", "diff", &number, "--repo", repo, "--color=never"],
        None,
    )?;
    Ok(PatchText {
        text: String::from_utf8_lossy(&out).into_owned(),
        reduced: false,
    })
}

#[cfg(test)]
mod tests;
