//! What the Changes panel's diff shows and what the reviewer has said about
//! it: each comparison parsed into Kit diff files, and line notes bound for
//! the pane's agent.
//!
//! Each file keeps its own patch section. That one text is what line staging
//! cuts, what a viewed mark fingerprints (so the mark lapses when the agent
//! touches the file again), and what tells a reload apart from no change.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use gpui::{Entity, Subscription};
use gpui_kit::component::diff::{DiffFile, DiffLineRange, DiffSide};
use gpui_kit::component::input::TextareaState;

use crate::git::{self, PatchText, Source};

/// Context fetched around each change: what the view's expand controls reveal.
pub(crate) const FETCH_CONTEXT: usize = 100_000;
/// Context shown around each change before a fold.
pub(crate) const SHOWN_CONTEXT: usize = 3;

/// One comparison, parsed for the view.
#[derive(Clone, Default)]
pub(crate) struct Loaded {
    pub files: Vec<DiffFile>,
    /// Each file's patch section (a conflicted file's working text), by path.
    pub sections: HashMap<String, String>,
    pub reduced: bool,
    /// Files the view could not read, by path.
    pub unreadable: Vec<String>,
}

impl Loaded {
    /// Conflicted working files first, then one Kit file per patch section.
    pub(crate) fn parse(patch: PatchText, conflicts: Vec<(String, String)>) -> Self {
        let mut loaded = Self {
            reduced: patch.reduced,
            ..Self::default()
        };
        for (path, text) in conflicts {
            loaded.add(
                DiffFile::parse_conflicts(path.clone(), &text).ok(),
                path,
                text,
            );
        }
        for section in git::split_files(&patch.text) {
            let file = DiffFile::parse(section)
                .ok()
                .and_then(|files| files.into_iter().next());
            let path = file
                .as_ref()
                .map_or_else(|| first_line(section), |f| f.path().to_string());
            loaded.add(file, path, section.to_owned());
        }
        loaded
    }

    fn add(&mut self, file: Option<DiffFile>, path: String, section: String) {
        match file {
            Some(file) => {
                self.files.push(file);
                self.sections.insert(path, section);
            }
            None => self.unreadable.push(path),
        }
    }

    /// Whether `other` shows exactly the same files.
    pub(crate) fn same(&self, other: &Self) -> bool {
        self.sections == other.sections && self.unreadable == other.unreadable
    }

    pub(crate) fn print(&self, path: &str) -> Option<u64> {
        let mut hasher = DefaultHasher::new();
        self.sections.get(path)?.hash(&mut hasher);
        Some(hasher.finish())
    }

    pub(crate) fn stats(&self, path: &str) -> Option<(usize, usize)> {
        self.files
            .iter()
            .find(|file| file.path().as_ref() == path)
            .map(|file| (file.additions(), file.deletions()))
    }
}

fn first_line(section: &str) -> String {
    section.lines().next().unwrap_or_default().to_owned()
}

/// A note on some lines of one comparison, for the agent.
#[derive(Clone, Debug)]
pub(crate) struct Comment {
    pub id: u64,
    pub source: Source,
    pub range: DiffLineRange,
    pub body: String,
}

/// The note being written, under the lines it is about.
pub(crate) struct Draft {
    pub source: Source,
    pub range: DiffLineRange,
    pub input: Entity<TextareaState>,
    pub _subscription: Subscription,
}

/// `path:12` or `path:12-15`; old-side lines say so. A range across both
/// sides names the new side's end.
pub(crate) fn range_label(range: &DiffLineRange) -> String {
    let path = range.path();
    if range.side() != range.end_side() {
        let new = match range.end_side() {
            DiffSide::Modified => range.end(),
            DiffSide::Original => range.start(),
        };
        return format!("{path}:{new}");
    }
    let (low, high) = (
        range.start().min(range.end()),
        range.start().max(range.end()),
    );
    let lines = if low == high {
        low.to_string()
    } else {
        format!("{low}-{high}")
    };
    let removed = if range.side() == DiffSide::Original {
        " (removed lines)"
    } else {
        ""
    };
    format!("{path}:{lines}{removed}")
}

/// The review as one message for the agent, in note order.
pub(crate) fn review_prompt(repo: &str, comments: &[Comment]) -> String {
    let mut prompt = format!("Review notes on the uncommitted changes in {repo}:\n");
    for (ix, comment) in comments.iter().enumerate() {
        let staged = if comment.source == Source::Staged {
            " (staged)"
        } else {
            ""
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
    prompt + "\nAddress each note, then summarize what you changed."
}

/// The git selection the kit's selected range describes.
pub(crate) fn selection(range: &DiffLineRange) -> git::Selection {
    let side = |side| match side {
        DiffSide::Original => git::Side::Old,
        DiffSide::Modified => git::Side::New,
    };
    git::Selection::lines(side(range.side()), range.start(), range.end())
        .with_end_side(side(range.end_side()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "diff --git a/a.rs b/a.rs\nindex 1..2 100644\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/b.rs b/b.rs\nnew file mode 100644\n--- /dev/null\n+++ b/b.rs\n@@ -0,0 +1 @@\n+fresh\n";

    fn parse(text: &str, conflicts: &[(&str, &str)]) -> Loaded {
        let patch = PatchText {
            text: text.into(),
            reduced: false,
        };
        let conflicts = conflicts
            .iter()
            .map(|(p, t)| ((*p).into(), (*t).into()))
            .collect();
        Loaded::parse(patch, conflicts)
    }

    fn paths(loaded: &Loaded) -> Vec<String> {
        loaded
            .files
            .iter()
            .map(|file| file.path().to_string())
            .collect()
    }

    #[test]
    fn each_section_is_a_file_with_its_own_fingerprint() {
        let loaded = parse(PATCH, &[]);
        assert_eq!(paths(&loaded), ["a.rs", "b.rs"]);
        assert_eq!(loaded.stats("a.rs"), Some((1, 1)));
        assert!(loaded.sections["b.rs"].starts_with("diff --git a/b.rs"));

        let edited = parse(&PATCH.replace("+fresh", "+fresher"), &[]);
        assert_eq!(loaded.print("a.rs"), edited.print("a.rs"));
        assert_ne!(loaded.print("b.rs"), edited.print("b.rs"));
        assert!(!loaded.same(&edited) && loaded.same(&parse(PATCH, &[])));
    }

    #[test]
    fn conflicted_files_lead() {
        let text = "<<<<<<< ours\nmine\n=======\ntheirs\n>>>>>>> theirs\n";
        let loaded = parse(PATCH, &[("c.rs", text)]);
        assert_eq!(paths(&loaded), ["c.rs", "a.rs", "b.rs"]);
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
