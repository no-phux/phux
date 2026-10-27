//! The review as one message for the agent working on the branch.

use crate::git::github::{LineSide, Thread};

use super::super::changes::review::range_label;
use super::Pending;

/// What the agent is told about.
pub(crate) struct Review<'a> {
    pub repo: &'a str,
    pub number: u64,
    pub branch: &'a str,
    pub url: &'a str,
    /// The verdict, when it is not a plain comment.
    pub verdict: Option<&'a str>,
    pub summary: &'a str,
    pub notes: &'a [Pending],
    /// Threads still open on GitHub, from anyone.
    pub open_threads: &'a [&'a Thread],
}

/// `path:12` for a thread, its old-side lines said so.
fn thread_label(thread: &Thread) -> String {
    let line = thread.line.or(thread.original_line).unwrap_or_default();
    let lines = match thread.start {
        Some((_, start)) if start < line => format!("{start}-{line}"),
        _ => line.to_string(),
    };
    let removed = if thread.side == LineSide::Left {
        " (removed lines)"
    } else {
        ""
    };
    let outdated = if thread.outdated { " (outdated)" } else { "" };
    format!("{}:{lines}{removed}{outdated}", thread.path)
}

fn indented(prompt: &mut String, body: &str) {
    for line in body.trim().lines() {
        prompt.push_str("   ");
        prompt.push_str(line);
        prompt.push('\n');
    }
}

pub(crate) fn prompt(review: &Review<'_>) -> String {
    let mut prompt = format!(
        "Review of pull request #{} in {} (branch {}): {}\n",
        review.number, review.repo, review.branch, review.url
    );
    if let Some(verdict) = review.verdict {
        prompt.push_str(&format!("Verdict: {verdict}\n"));
    }
    if !review.summary.trim().is_empty() {
        prompt.push('\n');
        indented(&mut prompt, review.summary);
    }
    for (ix, note) in review.notes.iter().enumerate() {
        prompt.push_str(&format!("\n{}. {}\n", ix + 1, range_label(&note.range)));
        indented(&mut prompt, &note.body);
    }
    if !review.open_threads.is_empty() {
        prompt.push_str("\nAlso unresolved on GitHub:\n");
        for thread in review.open_threads {
            let Some(first) = thread.comments.first() else {
                continue;
            };
            prompt.push_str(&format!(
                "\n- {} ({})\n",
                thread_label(thread),
                first.author
            ));
            indented(&mut prompt, &first.body);
        }
    }
    prompt.push_str("\nAddress each note on this branch, push, then summarize what you changed.");
    // Comments, paths and names come from GitHub: no control character may
    // end the bracketed paste or steer the terminal.
    prompt
        .chars()
        .filter(|ch| matches!(ch, '\n' | '\t') || !ch.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::diff::{DiffLineRange, DiffSide};

    use crate::git::github::{Note, Rights};

    use super::*;

    #[test]
    fn the_prompt_names_the_branch_notes_and_open_threads() {
        let notes = [Pending {
            id: 1,
            range: DiffLineRange::new("src/a.rs", DiffSide::Modified, 3, 5),
            body: "Split this.".into(),
        }];
        let thread = Thread {
            id: "T".into(),
            path: "b.rs".into(),
            resolved: false,
            outdated: true,
            side: LineSide::Left,
            line: None,
            original_line: Some(9),
            start: None,
            rights: Rights::default(),
            comments: vec![Note {
                id: "C".into(),
                author: "bot".into(),
                body: "Why?\u{1b}[201~".into(),
                created_at: String::new(),
            }],
        };
        let threads = [&thread];
        let review = Review {
            repo: "o/r",
            number: 7,
            branch: "feat/x",
            url: "https://github.com/o/r/pull/7",
            verdict: Some("changes requested"),
            summary: "Close.\n",
            notes: &notes,
            open_threads: &threads,
        };
        assert_eq!(
            prompt(&review),
            "Review of pull request #7 in o/r (branch feat/x): https://github.com/o/r/pull/7\n\
             Verdict: changes requested\n\
             \n   Close.\n\
             \n1. src/a.rs:3-5\n   Split this.\n\
             \nAlso unresolved on GitHub:\n\
             \n- b.rs:9 (removed lines) (outdated) (bot)\n   Why?[201~\n\
             \nAddress each note on this branch, push, then summarize what you changed."
        );
    }
}
