//! What the Changes panel's diff shows and what the reviewer has said about
//! it: each comparison's patch parsed into Kit diff files, which files are
//! marked viewed, and line comments bound for the pane's agent.
//!
//! A viewed mark remembers the file's patch fingerprint, so it lapses the
//! moment the agent touches the file again, as on GitHub. Comments stay in the
//! panel until sent; sending pastes one numbered review into the followed
//! terminal without pressing Enter, so the reviewer can still edit it there.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use gpui::{Entity, Subscription};
use gpui_kit::component::diff::{DiffFile, DiffLineRange, DiffSide};
use gpui_kit::component::input::TextareaState;

use crate::git::{self, PatchText, Source};

/// Context fetched around each change. The view folds all but
/// [`SHOWN_CONTEXT`] lines, and fetched context is what its expand controls reveal.
pub(crate) const FETCH_CONTEXT: usize = 100_000;
/// Context shown around each change before a fold.
pub(crate) const SHOWN_CONTEXT: usize = 3;

/// One comparison's patch, parsed for the view.
#[derive(Clone, Default)]
pub(crate) struct Loaded {
    /// The raw patch, which line staging cuts from.
    pub text: String,
    pub files: Vec<DiffFile>,
    /// Each file's fingerprint, keyed by path.
    pub prints: HashMap<String, u64>,
    /// Fingerprint of the whole comparison; equal means nothing to repaint.
    pub key: u64,
    /// Git returned a reduced context to stay within the size cap.
    pub reduced: bool,
    /// Files whose patch the view could not read, by path.
    pub unreadable: Vec<String>,
}

impl Loaded {
    pub(crate) fn stats(&self, path: &str) -> Option<(usize, usize)> {
        self.files
            .iter()
            .find(|file| file.path().as_ref() == path)
            .map(|file| (file.additions(), file.deletions()))
    }
}

/// Parse `patch` into one Kit file per section, conflicted working files first.
/// Combined (`diff --cc`) sections are left out: an unmerged path shows as its
/// conflict markers instead.
pub(crate) fn parse(patch: PatchText, conflicts: &[(String, String)]) -> Loaded {
    let mut loaded = Loaded {
        reduced: patch.reduced,
        ..Loaded::default()
    };
    let mut whole = DefaultHasher::new();
    for (path, text) in conflicts {
        (path, text).hash(&mut whole);
        match DiffFile::parse_conflicts(path.clone(), text) {
            Ok(file) => {
                loaded.prints.insert(path.clone(), fingerprint(text));
                loaded.files.push(file);
            }
            Err(_) => loaded.unreadable.push(path.clone()),
        }
    }
    patch.text.hash(&mut whole);
    for section in sections(&patch.text) {
        if section.starts_with("diff --cc ") || section.starts_with("diff --combined ") {
            continue;
        }
        match DiffFile::parse(section) {
            Ok(files) => {
                for file in files {
                    loaded
                        .prints
                        .insert(file.path().to_string(), fingerprint(section));
                    loaded.files.push(file);
                }
            }
            Err(_) => loaded.unreadable.push(section_path(section)),
        }
    }
    loaded.key = whole.finish();
    loaded.text = patch.text;
    loaded
}

/// The patch split before each `diff ` header line.
fn sections(text: &str) -> Vec<&str> {
    let mut starts: Vec<usize> = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.starts_with("diff ") {
            starts.push(offset);
        }
        offset += line.len();
    }
    starts
        .iter()
        .enumerate()
        .map(|(ix, start)| &text[*start..starts.get(ix + 1).copied().unwrap_or(text.len())])
        .collect()
}

/// The `b/` path of a section's header, for naming one the view can't read.
fn section_path(section: &str) -> String {
    let header = section.lines().next().unwrap_or_default();
    header
        .rsplit_once(" b/")
        .map_or(header, |(_, path)| path)
        .to_owned()
}

fn fingerprint(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// A note on some lines of one comparison, for the agent.
#[derive(Clone, Debug)]
pub(crate) struct Comment {
    pub id: u64,
    pub source: Source,
    pub range: DiffLineRange,
    pub body: String,
}

/// The comment being written, under the lines it is about.
pub(crate) struct Draft {
    pub source: Source,
    pub range: DiffLineRange,
    pub input: Entity<TextareaState>,
    pub _subscription: Subscription,
}

/// `path:12` or `path:12-15`, saying when the lines are the old version's.
pub(crate) fn range_label(range: &DiffLineRange) -> String {
    let (low, high) = (
        range.start().min(range.end()),
        range.start().max(range.end()),
    );
    let lines = if range.side() != range.end_side() {
        // Spans removed and added lines: name the new side's line.
        let new = match range.end_side() {
            DiffSide::Modified => range.end(),
            DiffSide::Original => range.start(),
        };
        format!("{new}")
    } else if low == high {
        format!("{low}")
    } else {
        format!("{low}-{high}")
    };
    let old = range.side() == DiffSide::Original && range.end_side() == DiffSide::Original;
    format!(
        "{}:{lines}{}",
        range.path(),
        if old { " (removed lines)" } else { "" }
    )
}

/// The review as one message for the agent, in comment order.
pub(crate) fn review_prompt(repo: &str, comments: &[Comment]) -> String {
    let mut prompt = format!("Review notes on the uncommitted changes in {repo}:\n");
    for (ix, comment) in comments.iter().enumerate() {
        let staged = match comment.source {
            Source::Staged => " (staged)",
            Source::Changes => "",
        };
        prompt.push_str(&format!(
            "\n{}. {}{staged}\n",
            ix + 1,
            range_label(&comment.range)
        ));
        for line in comment.body.trim().lines() {
            prompt.push_str(&format!("   {line}\n"));
        }
    }
    prompt.push_str("\nAddress each note, then summarize what you changed.");
    prompt
}

/// The git selection the kit's selected range describes.
pub(crate) fn selection(range: &DiffLineRange) -> git::Selection {
    let side = |side| match side {
        DiffSide::Original => git::Side::Old,
        DiffSide::Modified => git::Side::New,
    };
    git::Selection {
        path: range.path().to_string(),
        side: side(range.side()),
        start: range.start(),
        end_side: side(range.end_side()),
        end: range.end(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "diff --git a/a.rs b/a.rs\nindex 1..2 100644\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/b.rs b/b.rs\nnew file mode 100644\n--- /dev/null\n+++ b/b.rs\n@@ -0,0 +1 @@\n+fresh\n";

    fn patch(text: &str) -> PatchText {
        PatchText {
            text: text.into(),
            reduced: false,
        }
    }

    #[test]
    fn each_section_is_a_file_with_its_own_fingerprint() {
        let loaded = parse(patch(PATCH), &[]);
        let paths: Vec<_> = loaded.files.iter().map(|f| f.path().to_string()).collect();
        assert_eq!(paths, ["a.rs", "b.rs"]);
        assert_eq!(loaded.stats("a.rs"), Some((1, 1)));
        assert_eq!(loaded.stats("b.rs"), Some((1, 0)));

        let edited = parse(patch(&PATCH.replace("+fresh", "+fresher")), &[]);
        assert_eq!(loaded.prints["a.rs"], edited.prints["a.rs"]);
        assert_ne!(loaded.prints["b.rs"], edited.prints["b.rs"]);
        assert_ne!(loaded.key, edited.key);
    }

    #[test]
    fn conflicts_lead_and_combined_sections_are_skipped() {
        let combined =
            "diff --cc c.rs\nindex 1,2..3\n--- a/c.rs\n+++ b/c.rs\n@@@ -1,1 -1,1 +1,5 @@@\n";
        let text = "<<<<<<< ours\nmine\n=======\ntheirs\n>>>>>>> theirs\n";
        let loaded = parse(
            patch(&format!("{combined}{PATCH}")),
            &[("c.rs".into(), text.into())],
        );
        let paths: Vec<_> = loaded.files.iter().map(|f| f.path().to_string()).collect();
        assert_eq!(paths, ["c.rs", "a.rs", "b.rs"]);
        assert!(loaded.unreadable.is_empty());
    }

    #[test]
    fn the_prompt_numbers_notes_with_their_lines() {
        let comments = [
            Comment {
                id: 1,
                source: Source::Changes,
                range: DiffLineRange::new("src/a.rs", DiffSide::Modified, 14, 12),
                body: "Rename this.\nIt reads as a getter.".into(),
            },
            Comment {
                id: 2,
                source: Source::Staged,
                range: DiffLineRange::new("b.rs", DiffSide::Original, 3, 3),
                body: "Why drop this?".into(),
            },
        ];
        assert_eq!(
            review_prompt("phux", &comments),
            "Review notes on the uncommitted changes in phux:\n\
             \n1. src/a.rs:12-14\n   Rename this.\n   It reads as a getter.\n\
             \n2. b.rs:3 (removed lines) (staged)\n   Why drop this?\n\
             \nAddress each note, then summarize what you changed."
        );
    }
}
