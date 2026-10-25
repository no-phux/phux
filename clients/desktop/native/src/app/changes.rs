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
mod checkout;
use checkout::{expand_home, read_checkout};
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
        self.disclose(super::desk::CHANGES_DISCLOSURE.into(), cx);
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
        let executor = cx.background_executor().clone();
        self.changes._poll = Some(cx.spawn(async move |this, cx| {
            loop {
                executor.timer(POLL).await;
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
        if cfg!(windows) {
            return Err("This terminal runs in WSL. Review and stage its changes with Git in the terminal; the local Changes panel cannot read WSL paths.".into());
        }
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
mod tests;
