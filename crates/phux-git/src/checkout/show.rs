//! One commit in full.

use super::parse::{parse_commit, parse_numstat, text_at_line};
use super::types::Result;
use super::{
    Checkout, CheckoutError, CommitDetail, DIFF_FLAGS, LOG_FORMAT, MAX_ENTRIES, MAX_LISTING_OUTPUT,
    MAX_TEXT_BYTES,
};

/// Merges show what they brought in: the change against the first parent.
const FIRST_PARENT: &str = "--diff-merges=first-parent";

impl Checkout {
    /// The commit `sha` names (4 to 64 hex digits, a prefix included): its
    /// whole message, the files it changed against its first parent with
    /// line counts, and the patch.
    pub fn show(&self, sha: &str) -> Result<CommitDetail> {
        if !(4..=64).contains(&sha.len()) || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(CheckoutError::InvalidRequest(format!(
                "{sha:?} is not a commit id"
            )));
        }
        let rev = format!("{sha}^{{commit}}");
        let found = self.git(
            &["rev-parse", "-q", "--verify", "--end-of-options", &rev],
            4096,
        )?;
        if !found.status.success() {
            return Err(CheckoutError::NotFound(format!("no commit {sha}")));
        }
        let full = String::from_utf8_lossy(&found.stdout).trim().to_owned();

        // The subject's record, then the whole message after a separator.
        let format = format!("--format={LOG_FORMAT}%x1e%B");
        let head = self.git_ok(
            &[
                "-c",
                "log.showSignature=false",
                "show",
                "-s",
                "--no-color",
                &format,
                &full,
                "--",
            ],
            MAX_TEXT_BYTES,
        )?;
        let text = String::from_utf8_lossy(&head.stdout);
        let (record, message) = text.split_once('\x1e').unwrap_or((&text, ""));
        let commit = parse_commit(record.trim_start_matches('\n'))
            .ok_or_else(|| CheckoutError::Git(format!("git did not describe {sha}")))?;

        let numstat = self.git_ok(
            &[
                "show",
                "--format=",
                "--numstat",
                "-z",
                "--no-ext-diff",
                "--no-textconv",
                FIRST_PARENT,
                &full,
                "--",
            ],
            MAX_LISTING_OUTPUT,
        )?;
        let mut files = parse_numstat(&numstat.stdout);
        let mut truncated = numstat.truncated || files.len() > MAX_ENTRIES;
        files.truncate(MAX_ENTRIES);

        let mut args = vec!["show", "--format=", FIRST_PARENT];
        args.extend(DIFF_FLAGS);
        args.extend([full.as_str(), "--"]);
        let patch = self.git_ok(&args, MAX_TEXT_BYTES)?;
        let (diff, cut) = text_at_line(&patch.stdout, patch.truncated);
        truncated |= cut;

        Ok(CommitDetail {
            commit,
            message: message.trim_end_matches('\n').to_owned(),
            files,
            diff,
            truncated,
        })
    }
}
