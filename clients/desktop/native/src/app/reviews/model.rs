//! What the surface holds: a pull request's key, what was read for it, and
//! the review being written on it.

use std::collections::{HashMap, HashSet};

use gpui::{Entity, Subscription};
use gpui_kit::component::diff::{DiffLineRange, DiffState};
use gpui_kit::component::input::TextareaState;
use phux_protocol::ids::ResourceId;

use crate::git::github::{InboxPull, MergeMethod, Pull, ReviewEvent, ViewedState};

use super::super::changes::review::Loaded;

/// One pull request, by where it lives.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    pub repo: String,
    pub number: u64,
}

impl Key {
    pub(crate) fn of(pull: &InboxPull) -> Self {
        Self {
            repo: pull.repo.clone(),
            number: pull.number,
        }
    }

    pub(crate) fn label(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }
}

/// Something read from GitHub: the last answer is kept while a fresh one loads.
#[derive(Clone, Debug)]
pub(crate) struct Fetched<T> {
    pub data: Option<T>,
    pub loading: bool,
    pub error: Option<String>,
}

impl<T> Default for Fetched<T> {
    fn default() -> Self {
        Self {
            data: None,
            loading: false,
            error: None,
        }
    }
}

impl<T> Fetched<T> {
    pub(super) fn start(&mut self) {
        self.loading = true;
    }

    pub(super) fn finish(&mut self, result: Result<T, String>) {
        self.loading = false;
        match result {
            Ok(data) => {
                self.data = Some(data);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
}

/// Which pull requests the inbox lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Filter {
    #[default]
    All,
    /// Opened by you or an agent pushing as you.
    Mine,
    /// Awaiting your review.
    Requested,
    /// Everyone else's, in accounts you own.
    Others,
}

impl Filter {
    pub(crate) fn admits(self, pull: &InboxPull) -> bool {
        match self {
            Self::All => true,
            Self::Mine => pull.viewer_authored,
            Self::Requested => pull.reasons.requested,
            Self::Others => !pull.viewer_authored,
        }
    }
}

/// The opened pull request's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Tab {
    #[default]
    Files,
    Conversation,
    Checks,
}

/// Where the shown patch came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PatchFrom {
    /// A clone on this Mac: every line of context.
    Local(std::path::PathBuf),
    /// GitHub's patch: three lines of context.
    GitHub,
}

/// The patch, parsed for Kit's diff view.
#[derive(Clone)]
pub(crate) struct Patch {
    pub from: PatchFrom,
    pub loaded: Loaded,
    /// The head it was read at.
    pub head: String,
    pub base_ref: String,
    pub base_oid: String,
}

impl Patch {
    pub(super) fn matches(&self, pull: &Pull) -> bool {
        self.head == pull.head_oid
            && self.base_ref == pull.base_ref
            && self.base_oid == pull.base_oid
    }
}

/// A line comment kept for the review, not yet on GitHub.
#[derive(Clone, Debug)]
pub(crate) struct Pending {
    pub id: u64,
    pub range: DiffLineRange,
    pub body: String,
}

/// What the open text field writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DraftFor {
    /// A new comment on these lines, kept for the review.
    Lines(DiffLineRange),
    /// A reply on this thread, posted at once.
    Reply(String),
}

pub(crate) struct Draft {
    pub target: DraftFor,
    pub input: Entity<TextareaState>,
    pub _subscription: Subscription,
}

/// The agent pane working on the pull request's branch, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Agent {
    pub terminal: ResourceId,
    pub title: String,
}

/// Local checkouts that matter to the opened pull request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Place {
    /// A clone of the base repository, with the remote that names it.
    pub clone: Option<(std::path::PathBuf, String)>,
    /// A worktree already on the head branch.
    pub worktree: Option<std::path::PathBuf>,
    pub agent: Option<Agent>,
}

/// A failed job's log, by job.
#[derive(Clone, Debug)]
pub(crate) struct Log {
    pub job: u64,
    pub text: Fetched<String>,
}

/// An unsent review, kept while another pull request is open.
pub(crate) struct Kept {
    pub(super) pending: Vec<Pending>,
    pub(super) next_pending: u64,
    pub(super) summary: String,
    pub(super) event: ReviewEvent,
    pub(super) to_agent: bool,
    pub(super) draft: Option<Draft>,
    pub(super) patch: Option<Patch>,
}

/// The pull request shown in full.
pub(crate) struct Opened {
    pub key: Key,
    pub pull: Fetched<Pull>,
    pub patch: Fetched<Patch>,
    pub diff: Option<(Entity<DiffState>, Subscription)>,
    pub place: Place,
    pub tab: Tab,
    /// The file last revealed, kept in view across reloads.
    pub focus: Option<String>,
    /// Viewed marks set here and not yet confirmed by a reread.
    pub viewed: HashMap<String, bool>,
    /// Resolved threads shown open.
    pub expanded: HashSet<String>,
    pub pending: Vec<Pending>,
    pub(super) next_pending: u64,
    pub draft: Option<Draft>,
    pub summary: Option<(Entity<TextareaState>, Subscription)>,
    /// The summary to put in the field when it is next made.
    pub(super) kept_summary: String,
    pub event: ReviewEvent,
    /// Paste the notes into the branch's agent too.
    pub to_agent: bool,
    pub method: Option<MergeMethod>,
    /// The merge button was pressed once; the next press merges.
    pub confirm_merge: bool,
    /// A write is running; its name.
    pub busy: Option<&'static str>,
    pub log: Option<Log>,
    pub(super) generation: u64,
}

impl Opened {
    pub(super) fn new(key: Key, generation: u64) -> Self {
        Self {
            key,
            pull: Fetched::default(),
            patch: Fetched::default(),
            diff: None,
            place: Place::default(),
            tab: Tab::default(),
            focus: None,
            viewed: HashMap::new(),
            expanded: HashSet::new(),
            pending: Vec::new(),
            next_pending: 0,
            draft: None,
            summary: None,
            kept_summary: String::new(),
            event: ReviewEvent::Comment,
            to_agent: true,
            method: None,
            confirm_merge: false,
            busy: None,
            log: None,
            generation,
        }
    }

    /// Whether `path` is viewed: a mark set here wins over GitHub's.
    pub(crate) fn is_viewed(&self, path: &str) -> bool {
        if let Some(viewed) = self.viewed.get(path) {
            return *viewed;
        }
        self.pull.data.as_ref().is_some_and(|pull| {
            pull.files
                .iter()
                .any(|file| file.path == path && file.viewed == ViewedState::Viewed)
        })
    }

    /// Viewed then changed on GitHub since.
    pub(crate) fn is_dismissed(&self, path: &str) -> bool {
        !self.viewed.contains_key(path)
            && self.pull.data.as_ref().is_some_and(|pull| {
                pull.files
                    .iter()
                    .any(|file| file.path == path && file.viewed == ViewedState::Dismissed)
            })
    }

    /// Viewed files and every file the patch shows.
    pub(crate) fn progress(&self) -> (usize, usize) {
        let Some(patch) = &self.patch.data else {
            return (0, 0);
        };
        let files = &patch.loaded.files;
        let viewed = files
            .iter()
            .filter(|file| self.is_viewed(file.path()))
            .count();
        (viewed, files.len())
    }

    pub(crate) fn summary_text(&self, cx: &gpui::App) -> String {
        self.summary
            .as_ref()
            .map(|(input, _)| input.read(cx).value().trim().to_owned())
            .unwrap_or_else(|| self.kept_summary.clone())
    }

    /// An accepted write clears only the snapshot sent, never newer edits.
    pub(crate) fn finish_reply(&mut self, target: &DraftFor, body: &str, cx: &gpui::App) {
        let unchanged = self.draft.as_ref().is_some_and(|draft| {
            &draft.target == target && draft.input.read(cx).value().trim() == body
        });
        if unchanged {
            self.draft = None;
        }
    }

    pub(crate) fn finish_submission(
        &mut self,
        sent: &[u64],
        event: ReviewEvent,
        body: &str,
        cx: &gpui::App,
    ) {
        self.pending.retain(|pending| !sent.contains(&pending.id));
        if self.event == event {
            self.event = ReviewEvent::Comment;
        }
        if self.summary_text(cx) == body {
            self.summary = None;
            self.kept_summary.clear();
        }
    }

    /// Line notes stay attached to the exact diff they were written on.
    pub(super) fn has_line_notes(&self) -> bool {
        !self.pending.is_empty()
            || self
                .draft
                .as_ref()
                .is_some_and(|draft| matches!(draft.target, DraftFor::Lines(_)))
    }

    /// A reply's file can disappear after a push. Keep its composer reachable
    /// in the review bar when it cannot be drawn inline (or on another tab).
    pub(crate) fn draft_needs_bar(&self) -> bool {
        let Some(draft) = &self.draft else {
            return false;
        };
        if self.tab != Tab::Files {
            return true;
        }
        let DraftFor::Reply(id) = &draft.target else {
            return false;
        };
        let path = self.pull.data.as_ref().and_then(|pull| {
            pull.threads
                .iter()
                .find(|thread| &thread.id == id)
                .map(|thread| &thread.path)
        });
        !self.patch.data.as_ref().is_some_and(|patch| {
            patch
                .loaded
                .files
                .iter()
                .any(|file| Some(file.path().as_ref()) == path.map(String::as_str))
        })
    }

    pub(crate) fn revision_changed(&self) -> bool {
        matches!((&self.pull.data, &self.patch.data), (Some(pull), Some(patch)) if !patch.matches(pull))
    }

    pub(crate) fn base_changed(&self) -> bool {
        matches!((&self.pull.data, &self.patch.data), (Some(pull), Some(patch)) if patch.base_ref != pull.base_ref || patch.base_oid != pull.base_oid)
    }

    /// Disabling never depends on a diff or the repository's current methods.
    /// Enabling requires the loaded patch to describe the current head and base.
    pub(crate) fn auto_merge_change(&self) -> Option<Option<MergeMethod>> {
        let pull = self.pull.data.as_ref()?;
        if pull.auto_merge.is_some() {
            return Some(None);
        }
        if self.patch.loading || !self.patch.data.as_ref()?.matches(pull) {
            return None;
        }
        self.merge_method().map(Some)
    }

    /// Polling may refresh status, but must not move lines under unsent notes.
    pub(super) fn needs_patch(&self) -> bool {
        if self.patch.loading || self.has_line_notes() {
            return false;
        }
        self.pull.data.as_ref().is_some_and(|pull| {
            self.patch
                .data
                .as_ref()
                .is_none_or(|patch| !patch.matches(pull))
        })
    }

    /// Keep optimistic marks until GitHub confirms them, even if a read raced
    /// their write. The next poll then owns the value again.
    pub(super) fn reconcile_viewed(&mut self) {
        let Some(pull) = &self.pull.data else { return };
        self.viewed.retain(|path, viewed| {
            let on_github = pull
                .files
                .iter()
                .any(|file| &file.path == path && file.viewed == ViewedState::Viewed);
            on_github != *viewed
        });
    }

    pub(super) fn keep(self, cx: &gpui::App) -> Option<Kept> {
        let summary = self.summary_text(cx);
        if self.pending.is_empty()
            && self.draft.is_none()
            && summary.is_empty()
            && self.event == ReviewEvent::Comment
        {
            return None;
        }
        Some(Kept {
            patch: self.has_line_notes().then_some(self.patch.data).flatten(),
            pending: self.pending,
            next_pending: self.next_pending,
            summary,
            event: self.event,
            to_agent: self.to_agent,
            draft: self.draft,
        })
    }

    /// The merge method shown: the one picked here, else the repository's.
    pub(crate) fn merge_method(&self) -> Option<MergeMethod> {
        let pull = self.pull.data.as_ref()?;
        self.method
            .filter(|method| pull.methods.allowed.contains(method))
            .or_else(|| {
                pull.methods
                    .allowed
                    .contains(&pull.methods.preferred)
                    .then_some(pull.methods.preferred)
            })
    }
}
