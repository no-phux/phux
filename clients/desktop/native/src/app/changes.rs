//! The Changes panel's state: which checkout it follows, its last status, both
//! comparisons' patches, the diff view over one of them, and the review.
//!
//! The panel follows the focused pane's working directory, so an agent's
//! edits show beside the pane it runs in. Paths resolve only on this Mac: a
//! remote window or a satellite terminal shows why instead of reading some
//! other local folder. Every `git` call runs off the UI thread, parsing
//! included; a result for a folder the panel has since left is dropped. While
//! open, the panel re-reads on a short poll and repaints only when something
//! changed.
//!
//! | Module | Owns |
//! |---|---|
//! | [`tree`] | the file tree's rows |
//! | [`review`] | parsed patches, viewed marks, comments and the review prompt |
//! | [`navigate`] | which comparison and file the diff shows, viewed marks, folds |
//! | [`actions`] | staging, line staging, conflict resolution, commit, comments |

mod actions;
mod navigate;
pub(crate) mod review;
pub(crate) mod tree;

#[cfg(test)]
mod pipeline_tests;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use gpui::{Context, Entity, Focusable, Subscription, Task, Window};
use gpui_kit::component::diff::{DiffConflictResolution, DiffMode, DiffState};
use gpui_kit::component::input::TextareaState;
use phux_protocol::ids::ResourceId;

use crate::connection::path as paths;
use crate::git::{self, Area, Source, Status};

use super::desk::Desk;
use review::{Comment, Draft, Loaded};

/// How often an open panel re-reads the checkout.
const POLL: Duration = Duration::from_secs(3);
/// The panel's starting width, and its bounds while dragged.
pub(crate) const WIDTH: f32 = 760.;
pub(crate) const MIN_WIDTH: f32 = 480.;
/// Room the panes keep while the panel is dragged wider.
pub(crate) const PANES_MIN: f32 = 320.;

/// What the panel can show for the focused pane.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum View {
    /// Not read yet.
    #[default]
    Loading,
    /// The folder is a checkout.
    Repo(Status),
    /// The folder is not in a Git working tree.
    NotRepo(PathBuf),
    /// Nothing local to read, and why.
    Unavailable(String),
    Failed(String),
}

/// How the diff lays out the two sides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Layout {
    /// Side by side when the diff is wide enough, else one column.
    #[default]
    Auto,
    Split,
    Unified,
}

impl Layout {
    pub(crate) fn next(self) -> Self {
        match self {
            Self::Auto => Self::Split,
            Self::Split => Self::Unified,
            Self::Unified => Self::Auto,
        }
    }

    /// The mode for a diff `width` points wide.
    pub(crate) fn mode(self, width: f32) -> DiffMode {
        match self {
            Self::Split => DiffMode::Split,
            Self::Unified => DiffMode::Unified,
            Self::Auto if width >= SPLIT_WIDTH => DiffMode::Split,
            Self::Auto => DiffMode::Unified,
        }
    }
}

/// The narrowest diff an automatic layout splits.
const SPLIT_WIDTH: f32 = 820.;

/// Both comparisons of one read, parsed.
#[derive(Clone, Default)]
pub(crate) struct Comparisons {
    changes: Loaded,
    staged: Loaded,
}

impl Comparisons {
    pub(crate) fn get(&self, source: Source) -> &Loaded {
        match source {
            Source::Changes => &self.changes,
            Source::Staged => &self.staged,
        }
    }

    fn same(&self, other: &Self) -> bool {
        self.changes.same(&other.changes) && self.staged.same(&other.staged)
    }
}

pub(crate) struct Changes {
    pub open: bool,
    pub view: View,
    /// The folder the view was read for.
    folder: Option<PathBuf>,
    /// The checkout that owns review notes, independent of temporary loading states.
    review_root: Option<PathBuf>,
    /// The terminal the folder came from; a review is sent there.
    pub terminal: Option<ResourceId>,
    /// Bumped per read; a result carrying an older number is stale.
    generation: u64,
    reading: bool,
    /// The comparison the diff shows.
    pub source: Source,
    comparisons: Comparisons,
    pub diff: Option<(Entity<DiffState>, Subscription)>,
    /// Small authored choices, retained without the parsed diff while closed.
    suspended_conflicts: Vec<SuspendedConflict>,
    /// The file last revealed, kept in view across reloads.
    pub focus: Option<String>,
    /// Viewed files, per comparison, and the fingerprint each was viewed at.
    viewed: HashMap<(Source, String), u64>,
    /// Folded folders, by [`tree::Row::Folder`] key.
    pub collapsed: HashSet<String>,
    /// Files under folders, or one flat list.
    pub tree: bool,
    pub layout: Layout,
    pub width: f32,
    pub comments: Vec<Comment>,
    pub draft: Option<Draft>,
    next_comment: u64,
    /// An operation (stage, unstage, commit) is running.
    pub busy: bool,
    /// A commit landed; the field empties when next prepared.
    pending_clear: bool,
    pub message: Option<(Entity<TextareaState>, Subscription)>,
    _poll: Option<Task<()>>,
}

struct SuspendedConflict {
    path: String,
    fingerprint: u64,
    choices: Vec<(usize, DiffConflictResolution)>,
}

impl Default for Changes {
    fn default() -> Self {
        Self {
            open: false,
            view: View::default(),
            folder: None,
            review_root: None,
            terminal: None,
            generation: 0,
            reading: false,
            source: Source::Changes,
            comparisons: Comparisons::default(),
            diff: None,
            suspended_conflicts: Vec::new(),
            focus: None,
            viewed: HashMap::new(),
            collapsed: HashSet::new(),
            tree: true,
            layout: Layout::default(),
            width: WIDTH,
            comments: Vec::new(),
            draft: None,
            next_comment: 0,
            busy: false,
            pending_clear: false,
            message: None,
            _poll: None,
        }
    }
}

impl Changes {
    fn suspend_conflicts(&mut self, cx: &gpui::App) {
        if !matches!(self.view, View::Repo(_)) {
            return;
        }
        let Some((state, _)) = &self.diff else {
            return;
        };
        let state = state.read(cx);
        self.suspended_conflicts = state
            .files()
            .iter()
            .filter_map(|file| {
                let path = file.path().to_string();
                let choices: Vec<_> = (0..file.conflicts().len())
                    .filter_map(|index| {
                        state
                            .conflict_resolution(&path, index)
                            .map(|choice| (index, choice))
                    })
                    .collect();
                if choices.is_empty() {
                    return None;
                }
                Some(SuspendedConflict {
                    fingerprint: self.loaded(self.source).print(&path)?,
                    path,
                    choices,
                })
            })
            .collect();
    }

    fn restore_conflicts(&mut self, state: &Entity<DiffState>, cx: &mut gpui::App) {
        // A folder change first installs an empty Loading view. Keep authored
        // choices until a successful read establishes the checkout identity.
        if !matches!(self.view, View::Repo(_)) {
            return;
        }
        let suspended = std::mem::take(&mut self.suspended_conflicts);
        let suspended: Vec<_> = suspended
            .into_iter()
            .filter(|saved| self.loaded(self.source).print(&saved.path) == Some(saved.fingerprint))
            .collect();
        state.update(cx, |state, cx| {
            for saved in suspended {
                for (index, choice) in saved.choices {
                    state.resolve_conflict(&saved.path, index, Some(choice), cx);
                }
            }
        });
    }

    /// Closing keeps authored review state, not parsed patches or Kit's copy
    /// of the diff. Fence an in-flight read before dropping those allocations.
    fn release_preview(&mut self) {
        self._poll = None;
        self.generation += 1;
        self.reading = false;
        self.diff = None;
        self.comparisons = Comparisons::default();
        self.view = View::Loading;
    }

    /// Whether a panel field holds the keyboard, so the terminal must not take it.
    pub(crate) fn has_keys(&self, window: &Window, cx: &gpui::App) -> bool {
        let focused =
            |input: &Entity<TextareaState>| input.read(cx).focus_handle(cx).is_focused(window);
        self.open
            && (self
                .message
                .as_ref()
                .is_some_and(|(input, _)| focused(input))
                || self
                    .draft
                    .as_ref()
                    .is_some_and(|draft| focused(&draft.input))
                || self
                    .diff
                    .as_ref()
                    .is_some_and(|(state, _)| state.focus_handle(cx).contains_focused(window, cx)))
    }

    pub(crate) fn status(&self) -> Option<&Status> {
        match &self.view {
            View::Repo(status) => Some(status),
            _ => None,
        }
    }

    pub(crate) fn loaded(&self, source: Source) -> &Loaded {
        self.comparisons.get(source)
    }

    /// Whether `path` is marked viewed in the shown comparison and unchanged since.
    pub(crate) fn is_viewed(&self, path: &str) -> bool {
        let mark = self.viewed.get(&(self.source, path.to_owned()));
        mark.is_some() && mark.copied() == self.loaded(self.source).print(path)
    }

    /// Drop the shown comparison's marks whose file changed or left, and
    /// return the paths still listed: they must unfold.
    pub(crate) fn lapse_viewed(&mut self) -> Vec<String> {
        // An empty loading/error preview is not evidence that a viewed file
        // changed. Compare fingerprints only after a successful checkout read.
        if !matches!(self.view, View::Repo(_)) {
            return Vec::new();
        }
        let (source, loaded) = (self.source, self.comparisons.get(self.source));
        let mut unfold = Vec::new();
        self.viewed.retain(|(side, path), print| {
            let lapsed = *side == source && loaded.print(path) != Some(*print);
            if lapsed && loaded.sections.contains_key(path) {
                unfold.push(path.clone());
            }
            !lapsed
        });
        unfold
    }

    /// Mark `path` viewed at its current fingerprint, or clear the mark.
    pub(crate) fn set_viewed(&mut self, path: &str, viewed: bool) {
        let key = (self.source, path.to_owned());
        match self.loaded(self.source).print(path).filter(|_| viewed) {
            Some(print) => self.viewed.insert(key, print),
            None => self.viewed.remove(&key),
        };
    }

    /// Viewed files of the shown comparison, and how many it has.
    pub(crate) fn progress(&self) -> (usize, usize) {
        let files = &self.loaded(self.source).files;
        let viewed = files
            .iter()
            .filter(|file| self.is_viewed(file.path()))
            .count();
        (viewed, files.len())
    }

    /// Comments on the shown comparison.
    pub(crate) fn shown_comments(&self) -> impl Iterator<Item = &Comment> {
        self.comments
            .iter()
            .filter(move |comment| comment.source == self.source)
    }

    pub(crate) fn message_text(&self, cx: &gpui::App) -> String {
        self.message
            .as_ref()
            .map(|(input, _)| input.read(cx).value().to_string())
            .unwrap_or_default()
    }

    /// A commit can run: something is staged, nothing conflicts, and there is a message.
    pub(crate) fn can_commit(&self, cx: &gpui::App) -> bool {
        !self.busy
            && self.status().is_some_and(|status| {
                status.count(Area::Staged) > 0 && status.count(Area::Conflicted) == 0
            })
            && !self.message_text(cx).trim().is_empty()
    }
}

impl Desk {
    pub(crate) fn toggle_changes(&mut self, cx: &mut Context<Self>) {
        self.changes.open = !self.changes.open;
        if self.changes.open {
            self.refresh_changes(true, cx);
            self.arm_changes_poll(cx);
        } else {
            self.changes.suspend_conflicts(cx);
            self.changes.release_preview();
        }
        cx.notify();
    }

    fn arm_changes_poll(&mut self, cx: &mut Context<Self>) {
        self.changes._poll = Some(cx.spawn(async move |this, cx| {
            loop {
                smol::Timer::after(POLL).await;
                let Ok(open) = this.update(cx, |desk, cx| {
                    if desk.changes.open {
                        desk.refresh_changes(false, cx);
                    }
                    desk.changes.open
                }) else {
                    return;
                };
                if !open {
                    return;
                }
            }
        }));
    }

    /// Re-read when the focused pane now points at another folder (focus
    /// moved, or its shell changed directory). Another pane in the same
    /// checkout becomes where a review is sent.
    pub(crate) fn follow_changes(&mut self, cx: &mut Context<Self>) {
        if !self.changes.open {
            return;
        }
        if self.changes_folder().ok() != self.changes.folder {
            self.refresh_changes(true, cx);
        } else if let Some(terminal) = self.focused_terminal() {
            self.changes.terminal = Some(terminal);
        }
    }

    /// The local folder the focused pane is in, or why there is none.
    fn changes_folder(&self) -> Result<PathBuf, String> {
        if let Some(host) = self.remote.as_deref() {
            return Err(format!(
                "This window is on {host}. Changes are read from folders on this Mac."
            ));
        }
        let placement = self
            .focused_placement()
            .ok_or_else(|| String::from("Focus a terminal to see its changes."))?;
        if paths::host_for_terminal(&placement.terminal_label).is_some() {
            return Err("This terminal runs on another machine.".into());
        }
        let cwd = self
            .focused_pane()
            .and_then(|pane| pane.cwd.clone())
            .ok_or_else(|| String::from("This terminal hasn't reported its folder yet."))?;
        Ok(expand_home(&cwd))
    }

    /// Re-read the focused pane's checkout. `force` reads even while another
    /// read is in flight, such as after the focus moved.
    pub(crate) fn refresh_changes(&mut self, force: bool, cx: &mut Context<Self>) {
        if !self.changes.open || (self.changes.reading && !force) {
            return;
        }
        match self.changes_folder() {
            Ok(folder) => self.read_changes(folder, cx),
            Err(reason) => {
                self.changes.generation += 1;
                self.changes.reading = false;
                self.changes.terminal = None;
                self.changes.folder = None;
                self.set_changes_view(View::Unavailable(reason), Comparisons::default(), cx);
            }
        }
    }

    /// Read `folder` off the UI thread. A later read makes this one's result stale.
    fn read_changes(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        self.changes.terminal = self.focused_terminal();
        if self.changes.folder.as_ref() != Some(&folder) {
            self.changes.folder = Some(folder.clone());
            self.changes.focus = None;
            self.set_changes_view(View::Loading, Comparisons::default(), cx);
        }
        self.changes.generation += 1;
        self.changes.reading = true;
        let generation = self.changes.generation;
        cx.spawn(async move |this, cx| {
            let read = folder.clone();
            let result = smol::unblock(move || read_checkout(&read)).await;
            let _ = this.update(cx, |desk, cx| {
                if desk.changes.generation != generation {
                    return;
                }
                desk.changes.reading = false;
                let (view, comparisons) = match result {
                    Ok(Some((status, comparisons))) => (View::Repo(status), comparisons),
                    Ok(None) => (View::NotRepo(folder), Comparisons::default()),
                    Err(error) => (View::Failed(error), Comparisons::default()),
                };
                desk.set_changes_view(view, comparisons, cx);
            });
        })
        .detach();
    }

    /// Show `view` and its comparisons. The diff reloads only when the shown
    /// comparison changed, since a reload resets its scroll and folds. An
    /// emptied comparison gives way to the other.
    fn set_changes_view(&mut self, view: View, comparisons: Comparisons, cx: &mut Context<Self>) {
        if let View::Repo(status) = &view {
            self.changes.bind_review(&status.root);
        }
        if self.changes.view != view {
            self.changes.view = view;
            cx.notify();
        }
        let old = std::mem::replace(&mut self.changes.comparisons, comparisons);
        let was = self.changes.source;
        let fresh = &self.changes.comparisons;
        if fresh.get(was).files.is_empty() && !fresh.get(was.other()).files.is_empty() {
            self.changes.source = was.other();
        }
        let shown = self.changes.source;
        if shown != was
            || !old.get(shown).same(self.changes.loaded(shown))
            || self.changes.diff.is_none()
        {
            self.show_source(cx);
        } else if !old.same(&self.changes.comparisons) {
            cx.notify();
        }
    }
}

impl Changes {
    /// Never carry a review to another checkout's terminal or reuse its viewed marks.
    fn bind_review(&mut self, root: &std::path::Path) {
        if self.review_root.as_deref() == Some(root) {
            return;
        }
        self.comments.clear();
        self.draft = None;
        self.viewed.clear();
        self.suspended_conflicts.clear();
        self.review_root = Some(root.to_owned());
    }
}

#[cfg(test)]
impl Desk {
    /// Show `status` with these patches, as a read of a checkout would.
    pub(crate) fn show_checkout(
        &mut self,
        status: Status,
        changes: &str,
        staged: &str,
        cx: &mut Context<Self>,
    ) {
        let parse = |text: &str| {
            let patch = git::PatchText {
                text: text.to_owned(),
                reduced: false,
            };
            Loaded::parse(patch, Vec::new())
        };
        let comparisons = Comparisons {
            changes: parse(changes),
            staged: parse(staged),
        };
        self.set_changes_view(View::Repo(status), comparisons, cx);
    }
}

/// Status, then both comparisons, then the text of each conflicted file.
fn read_checkout(folder: &std::path::Path) -> Result<Option<(Status, Comparisons)>, String> {
    let Some(status) = git::read_status(folder)? else {
        return Ok(None);
    };
    let root = status.root.clone();
    let untracked: Vec<String> = status
        .in_area(Area::Untracked)
        .map(|entry| entry.path.clone())
        .collect();
    let conflicts: Vec<(String, String)> = status
        .in_area(Area::Conflicted)
        .filter_map(|entry| {
            git::read_file(&root, &entry.path)
                .ok()
                .map(|text| (entry.path.clone(), text))
        })
        .collect();
    let changes = git::read_patch(&root, Source::Changes, &untracked, review::FETCH_CONTEXT)?;
    let staged = git::read_patch(&root, Source::Staged, &[], review::FETCH_CONTEXT)?;
    let comparisons = Comparisons {
        changes: Loaded::parse(changes, conflicts),
        staged: Loaded::parse(staged, Vec::new()),
    };
    Ok(Some((status, comparisons)))
}

/// `~/x` as an absolute path, for a shell that reported its folder that way.
fn expand_home(cwd: &str) -> PathBuf {
    match (cwd.strip_prefix('~'), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            PathBuf::from(home).join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(cwd),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AppContext as _;

    #[test]
    fn closing_preview_fences_reads_and_preserves_review_state() {
        let mut changes = Changes {
            generation: 42,
            reading: true,
            ..Changes::default()
        };
        let root = PathBuf::from("/review");
        changes.bind_review(&root);
        changes.focus = Some("f.rs".into());
        changes.viewed.insert((Source::Changes, "f.rs".into()), 7);
        changes.collapsed.insert("changes:src".into());
        changes
            .comparisons
            .changes
            .sections
            .insert("f.rs".into(), "patch".repeat(1024));
        changes.release_preview();
        assert_eq!(
            changes.generation, 43,
            "an outstanding generation 42 read is stale"
        );
        assert!(!changes.reading);
        assert!(changes.comparisons.changes.sections.is_empty());
        assert_eq!(changes.review_root, Some(root));
        assert_eq!(changes.focus.as_deref(), Some("f.rs"));
        assert_eq!(changes.viewed[&(Source::Changes, "f.rs".into())], 7);
        assert!(changes.collapsed.contains("changes:src"));
        assert!(changes.lapse_viewed().is_empty());
        assert_eq!(changes.viewed[&(Source::Changes, "f.rs".into())], 7);
    }

    #[gpui::test]
    fn conflict_choices_survive_close_only_while_the_file_is_unchanged(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui_kit::component::diff::{DiffConflictResolution, DiffEvent};
        cx.update(gpui_kit::init);
        let text = "<<<<<<< ours\nmine\n=======\ntheirs\n>>>>>>> theirs\n";
        let loaded = || {
            Loaded::parse(
                git::PatchText::default(),
                vec![("f.rs".into(), text.into())],
            )
        };
        let mut changes = Changes::default();
        changes.comparisons.changes = loaded();
        changes.view = View::Repo(Status {
            root: PathBuf::from("/review"),
            branch: git::Branch::default(),
            entries: Vec::new(),
        });
        let files = changes.comparisons.changes.files.clone();
        let state = cx.new(|cx| DiffState::new(files, cx));
        state.update(cx, |state, cx| {
            state.resolve_conflict("f.rs", 0, Some(DiffConflictResolution::Current), cx)
        });
        let subscription = cx.update(|cx| cx.subscribe(&state, |_, _: &DiffEvent, _| {}));
        changes.diff = Some((state.clone(), subscription));
        cx.update(|cx| changes.suspend_conflicts(cx));
        changes.release_preview();
        assert_eq!(changes.suspended_conflicts.len(), 1);
        // Reopening from a different subdirectory first displays Loading.
        let loading = cx.new(|cx| DiffState::new(Vec::new(), cx));
        cx.update(|cx| changes.restore_conflicts(&loading, cx));
        assert_eq!(changes.suspended_conflicts.len(), 1);
        changes.view = View::Repo(Status {
            root: PathBuf::from("/review"),
            branch: git::Branch::default(),
            entries: Vec::new(),
        });
        changes.comparisons.changes = loaded();
        let files = changes.comparisons.changes.files.clone();
        let reopened = cx.new(|cx| DiffState::new(files, cx));
        cx.update(|cx| changes.restore_conflicts(&reopened, cx));
        assert_eq!(
            reopened
                .read_with(cx, |state, _| state.resolved_text("f.rs"))
                .as_deref(),
            Some("mine\n")
        );

        // Preserve nothing if the working conflict changed while closed.
        let subscription = cx.update(|cx| cx.subscribe(&reopened, |_, _: &DiffEvent, _| {}));
        changes.diff = Some((reopened, subscription));
        cx.update(|cx| changes.suspend_conflicts(cx));
        changes.release_preview();
        changes.comparisons.changes = Loaded::parse(
            git::PatchText::default(),
            vec![("f.rs".into(), text.replace("mine", "newer"))],
        );
        changes.view = View::Repo(Status {
            root: PathBuf::from("/review"),
            branch: git::Branch::default(),
            entries: Vec::new(),
        });
        let files = changes.comparisons.changes.files.clone();
        let changed = cx.new(|cx| DiffState::new(files, cx));
        cx.update(|cx| changes.restore_conflicts(&changed, cx));
        assert!(
            changed
                .read_with(cx, |state, _| state.conflict_resolution("f.rs", 0))
                .is_none()
        );
    }

    /// Observational only: RSS need not drop immediately when malloc retains pages.
    #[gpui::test]
    #[ignore = "isolated memory observation; run alone with --ignored --nocapture"]
    fn measure_closed_preview_memory(cx: &mut gpui::TestAppContext) {
        use gpui_kit::component::diff::DiffEvent;
        use phux_perf::ProcessStats;
        cx.update(gpui_kit::init);
        let before = ProcessStats::physical_footprint_bytes();
        let text = format!(
            "diff --git a/large.rs b/large.rs\n--- a/large.rs\n+++ b/large.rs\n@@ -1,50000 +1,50000 @@\n{}-old\n+new\n",
            " context line of a large source file\n".repeat(49_999)
        );
        let mut changes = Changes::default();
        changes.comparisons.changes = Loaded::parse(
            git::PatchText {
                text,
                reduced: false,
            },
            Vec::new(),
        );
        let patch_bytes: usize = changes
            .comparisons
            .changes
            .sections
            .values()
            .map(String::len)
            .sum();
        let files = changes.comparisons.changes.files.clone();
        let state = cx.new(|cx| DiffState::new(files, cx));
        let weak = state.downgrade();
        let subscription = cx.update(|cx| cx.subscribe(&state, |_, _: &DiffEvent, _| {}));
        changes.diff = Some((state, subscription));
        let loaded = ProcessStats::physical_footprint_bytes();
        changes.release_preview();
        cx.run_until_parked();
        assert!(weak.upgrade().is_none());
        assert!(changes.comparisons.changes.sections.is_empty());
        eprintln!(
            "preview memory bytes: baseline={before:?}, loaded={loaded:?}, closed={:?}, released_patch_text={patch_bytes}",
            ProcessStats::physical_footprint_bytes()
        );
    }

    #[test]
    fn review_notes_belong_to_one_checkout() {
        use gpui_kit::component::diff::{DiffLineRange, DiffSide};
        let mut changes = Changes::default();
        changes.bind_review(std::path::Path::new("/first"));
        changes.comments.push(Comment {
            id: 1,
            source: Source::Changes,
            range: DiffLineRange::new("f.rs", DiffSide::Modified, 1, 1),
            body: "Review only the first checkout".into(),
        });
        changes.bind_review(std::path::Path::new("/first"));
        assert_eq!(changes.comments.len(), 1);
        changes.bind_review(std::path::Path::new("/second"));
        assert!(changes.comments.is_empty());
    }

    #[test]
    fn a_home_relative_folder_expands_and_others_stay() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(expand_home("~"), home);
        assert_eq!(expand_home("~/src/phux"), home.join("src/phux"));
        assert_eq!(expand_home("/tmp/x"), PathBuf::from("/tmp/x"));
        assert_eq!(expand_home("~other/x"), PathBuf::from("~other/x"));
    }

    #[test]
    fn auto_layout_splits_only_a_wide_diff() {
        assert_eq!(Layout::Auto.mode(SPLIT_WIDTH - 1.), DiffMode::Unified);
        assert_eq!(Layout::Auto.mode(SPLIT_WIDTH), DiffMode::Split);
        assert_eq!(Layout::Unified.mode(2000.), DiffMode::Unified);
        assert_eq!(Layout::Split.mode(10.), DiffMode::Split);
        assert_eq!(Layout::Unified.next(), Layout::Auto);
    }
}
