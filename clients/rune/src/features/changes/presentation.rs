//! What the changes panel says: a checkout's answers as rows, diff lines and
//! plain words. Pure, so every state can be tested without a server.

use phux_client_runtime::control::{CheckoutFailure, CheckoutOutcome};
use phux_protocol::checkout::{
    CheckoutAnswer, CheckoutArea, CheckoutDiff, CheckoutEntry, CheckoutRefusal,
    CheckoutRefusalCode, CheckoutStatus,
};

/// Most diff lines drawn.
// ponytail: the panel draws every line it keeps, so a huge diff is cut here
// and says so; virtualize the lines if people review diffs longer than this.
pub const MAX_LINES: usize = 2000;

/// One changed file in one area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub path: String,
    pub area: CheckoutArea,
    /// Git's status letter, and the area in a word.
    pub code: String,
    pub state: &'static str,
    /// `+3 −1`, `binary`, or nothing when Git did not count.
    pub counts: String,
    /// The path a rename or copy came from.
    pub from: Option<String>,
}

/// A checkout's status as the panel lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    /// The branch, its upstream distance, and how many files changed where.
    pub summary: String,
    pub rows: Vec<Row>,
}

/// What a diff line is, for its tint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    /// File headers before the first hunk, and `\ No newline` notes.
    Meta,
    Hunk,
    Added,
    Removed,
    Context,
}

/// One file's diff: consecutive lines of a kind as one run of text, and
/// what was left out.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Diff {
    pub runs: Vec<(Line, String)>,
    pub note: Option<String>,
}

const fn area_word(area: CheckoutArea) -> &'static str {
    match area {
        CheckoutArea::Conflicted => "conflicted",
        CheckoutArea::Staged => "staged",
        CheckoutArea::Unstaged => "unstaged",
        CheckoutArea::Untracked => "untracked",
        CheckoutArea::Unknown => "changed",
    }
}

fn row(entry: &CheckoutEntry) -> Row {
    let counts = match (entry.binary, entry.added, entry.deleted) {
        (true, ..) => "binary".to_owned(),
        (false, Some(added), Some(deleted)) => format!("+{added} −{deleted}"),
        _ => String::new(),
    };
    Row {
        path: entry.path.clone(),
        area: entry.area,
        code: entry.code.clone(),
        state: area_word(entry.area),
        counts,
        from: entry.from.clone(),
    }
}

/// The status as rows, in the server's order (by area, then path).
pub fn listing(status: &CheckoutStatus) -> Listing {
    let mut parts = vec![status.branch.clone().unwrap_or_else(|| {
        let head = status.head.as_deref().unwrap_or_default();
        match head.get(..7) {
            Some(short) => format!("detached at {short}"),
            None => "no commits yet".to_owned(),
        }
    })];
    if status.ahead > 0 {
        parts.push(format!("{} ahead", status.ahead));
    }
    if status.behind > 0 {
        parts.push(format!("{} behind", status.behind));
    }
    for area in [
        CheckoutArea::Conflicted,
        CheckoutArea::Staged,
        CheckoutArea::Unstaged,
        CheckoutArea::Untracked,
        CheckoutArea::Unknown,
    ] {
        let count = status.entries.iter().filter(|e| e.area == area).count();
        if count > 0 {
            parts.push(format!("{count} {}", area_word(area)));
        }
    }
    if status.entries.is_empty() {
        parts.push("no changes".to_owned());
    }
    if status.truncated {
        parts.push("more files than the server lists".to_owned());
    }
    Listing {
        summary: parts.join(" · "),
        rows: status.entries.iter().map(row).collect(),
    }
}

/// A unified diff's lines by kind. `+++` and `---` are headers only before
/// the first hunk; after it they are content that starts with `++` or `--`.
pub fn classify(text: &str) -> Vec<(Line, &str)> {
    let mut in_hunk = false;
    text.lines()
        .map(|line| {
            let kind = match line.as_bytes().first() {
                Some(b'@') if line.starts_with("@@") => {
                    in_hunk = true;
                    Line::Hunk
                }
                Some(b'+') if in_hunk => Line::Added,
                Some(b'-') if in_hunk => Line::Removed,
                Some(b' ') | None if in_hunk => Line::Context,
                _ => Line::Meta,
            };
            (kind, line)
        })
        .collect()
}

/// A diff answer as runs, at most `max_lines` of them drawn, saying what
/// the server or this panel cut.
pub fn diff(answer: &CheckoutDiff, max_lines: usize) -> Diff {
    let lines = classify(&answer.text);
    let mut runs: Vec<(Line, String)> = Vec::new();
    for (kind, line) in lines.iter().take(max_lines) {
        match runs.last_mut() {
            Some((last, text)) if last == kind => {
                text.push('\n');
                text.push_str(line);
            }
            _ => runs.push((*kind, (*line).to_owned())),
        }
    }
    let note = if answer.binary {
        Some("Binary file; Git shows no text for it.".to_owned())
    } else if lines.len() > max_lines {
        Some(format!(
            "Showing the first {max_lines} of {} lines.",
            lines.len()
        ))
    } else if answer.truncated {
        Some("The diff is too large; this is the start of it.".to_owned())
    } else if lines.is_empty() {
        Some("No text changes in this file.".to_owned())
    } else {
        None
    };
    Diff { runs, note }
}

/// A typed refusal in words. The server's own message follows only where
/// ours can't say what went wrong.
pub fn refusal(refusal: &CheckoutRefusal) -> String {
    use CheckoutRefusalCode as Code;
    let words = match refusal.code {
        Code::CwdUnknown => "The server can't tell which folder this terminal is in.",
        Code::NotARepository => "Not a Git checkout: this terminal's folder is not in one.",
        Code::InvalidPath => "The server refused that path.",
        Code::NotFound => "That file is gone.",
        Code::Timeout => "Git did not finish in time. Refresh to try again.",
        Code::Busy => "The server is busy with other checkout work. Refresh to try again.",
        Code::InvalidRequest => {
            let words = "The server refused the request as out of bounds.";
            return with_detail(words, &refusal.message);
        }
        Code::GitFailed => return with_detail("Git failed.", &refusal.message),
        Code::ReadFailed => {
            let words = "The host could not read that (permission denied, perhaps).";
            return with_detail(words, &refusal.message);
        }
        Code::GhUnavailable
        | Code::GhUnauthenticated
        | Code::NoPullRequest
        | Code::GhFailed
        | Code::Unknown => {
            let words = "The server refused, for a reason this version doesn't know.";
            return with_detail(words, &refusal.message);
        }
    };
    words.to_owned()
}

/// A request that never reached the checkout, in words.
pub fn failure(failure: CheckoutFailure, message: &str) -> String {
    let words = match failure {
        CheckoutFailure::PermissionDenied => {
            "Permission denied: this connection may not read that terminal's checkout."
        }
        CheckoutFailure::TerminalNotFound => "That terminal is gone.",
        CheckoutFailure::Unsupported => "The machine this terminal runs on doesn't serve changes.",
        CheckoutFailure::Busy => "The server is busy. Refresh to try again.",
        CheckoutFailure::Unanswered => "The connection ended before the answer. Refresh to retry.",
        CheckoutFailure::Other => return with_detail("The server refused.", message),
    };
    words.to_owned()
}

/// Why nothing was sent: `checkout` answered zero.
pub const fn unsent(connected: bool) -> &'static str {
    if connected {
        "This server doesn't support changes. Update phux on that machine."
    } else {
        "Not connected; changes can't be read."
    }
}

fn with_detail(words: &str, detail: &str) -> String {
    match detail.trim() {
        "" => words.to_owned(),
        detail => format!("{words} {detail}"),
    }
}

/// The status an outcome carries, or why there is none.
pub fn status_of(outcome: &CheckoutOutcome) -> Result<Listing, String> {
    match answer_of(outcome)? {
        CheckoutAnswer::Status(status) => Ok(listing(status)),
        _ => Err("The server answered something other than a status.".to_owned()),
    }
}

/// The diff an outcome carries, or why there is none.
pub fn diff_of(outcome: &CheckoutOutcome) -> Result<Diff, String> {
    match answer_of(outcome)? {
        CheckoutAnswer::Diff(answer) => Ok(diff(answer, MAX_LINES)),
        _ => Err("The server answered something other than a diff.".to_owned()),
    }
}

fn answer_of(outcome: &CheckoutOutcome) -> Result<&CheckoutAnswer, String> {
    match outcome {
        CheckoutOutcome::Answered(answer) => match answer.as_ref() {
            CheckoutAnswer::Refused(refused) => Err(refusal(refused)),
            answer => Ok(answer),
        },
        CheckoutOutcome::Failed {
            failure: kind,
            message,
        } => Err(failure(*kind, message)),
    }
}

#[cfg(test)]
mod tests;
