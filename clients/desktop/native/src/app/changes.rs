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
use gpui_kit::component::diff::{DiffMode, DiffState};
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

/// One read of a checkout: its status and both comparisons, parsed.
struct Snapshot {
    status: Status,
    changes: Loaded,
    staged: Loaded,
}

pub(crate) struct Changes {
    pub open: bool,
    pub view: View,
    /// The folder the view was read for.
    folder: Option<PathBuf>,
    /// The terminal the folder came from; a review is sent there.
    pub terminal: Option<ResourceId>,
    /// Bumped per read; a result carrying an older number is stale.
    generation: u64,
    reading: bool,
    /// The comparison the diff shows.
    pub source: Source,
    changes: Loaded,
    staged: Loaded,
    pub diff: Option<(Entity<DiffState>, Subscription)>,
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

impl Default for Changes {
    fn default() -> Self {
        Self {
            open: false,
            view: View::default(),
            folder: None,
            terminal: None,
            generation: 0,
            reading: false,
            source: Source::Changes,
            changes: Loaded::default(),
            staged: Loaded::default(),
            diff: None,
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
        match source {
            Source::Changes => &self.changes,
            Source::Staged => &self.staged,
        }
    }

    /// Whether `path` is marked viewed in the shown comparison and unchanged since.
    pub(crate) fn is_viewed(&self, path: &str) -> bool {
        self.viewed
            .get(&(self.source, path.to_owned()))
            .is_some_and(|print| self.loaded(self.source).prints.get(path) == Some(print))
    }

    /// Drop the shown comparison's viewed marks whose file changed or left,
    /// returning the paths still listed, which must unfold.
    pub(crate) fn lapse_viewed(&mut self) -> Vec<String> {
        let source = self.source;
        let prints = match source {
            Source::Changes => &self.changes.prints,
            Source::Staged => &self.staged.prints,
        };
        let lapsed: Vec<String> = self
            .viewed
            .iter()
            .filter(|((side, path), print)| *side == source && prints.get(path) != Some(print))
            .map(|((_, path), _)| path.clone())
            .collect();
        for path in &lapsed {
            self.viewed.remove(&(source, path.clone()));
        }
        lapsed
            .into_iter()
            .filter(|path| prints.contains_key(path))
            .collect()
    }

    /// Mark `path` viewed at its current fingerprint, or clear the mark.
    pub(crate) fn set_viewed(&mut self, path: &str, viewed: bool) {
        let key = (self.source, path.to_owned());
        match self.loaded(self.source).prints.get(path).copied() {
            Some(print) if viewed => {
                self.viewed.insert(key, print);
            }
            _ => {
                self.viewed.remove(&key);
            }
        }
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
            self.changes._poll = None;
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
    /// moved, or its shell changed directory).
    /// Another pane in the same checkout becomes where a review is sent.
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
        let folder = match self.changes_folder() {
            Ok(folder) => folder,
            Err(reason) => {
                self.changes.folder = None;
                self.set_changes_view(View::Unavailable(reason), None, cx);
                return;
            }
        };
        self.changes.terminal = self.focused_terminal();
        if self.changes.folder.as_ref() != Some(&folder) {
            self.changes.folder = Some(folder.clone());
            self.changes.focus = None;
            self.set_changes_view(View::Loading, None, cx);
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
                match result {
                    Ok(Some(snapshot)) => {
                        let view = View::Repo(snapshot.status.clone());
                        desk.set_changes_view(view, Some(snapshot), cx);
                    }
                    Ok(None) => desk.set_changes_view(View::NotRepo(folder), None, cx),
                    Err(error) => desk.set_changes_view(View::Failed(error), None, cx),
                }
            });
        })
        .detach();
    }

    /// Show `view`, and the comparisons a checkout came with. Repaints only
    /// what changed: an unchanged patch keeps the diff's scroll and folds.
    fn set_changes_view(&mut self, view: View, snapshot: Option<Snapshot>, cx: &mut Context<Self>) {
        if self.changes.view != view {
            self.changes.view = view;
            cx.notify();
        }
        let (changes, staged) = snapshot.map_or_else(
            || (Loaded::default(), Loaded::default()),
            |snapshot| (snapshot.changes, snapshot.staged),
        );
        let fresh =
            |old: &Loaded, new: &Loaded| old.key != new.key || old.files.len() != new.files.len();
        let shown_changed = match self.changes.source {
            Source::Changes => fresh(&self.changes.changes, &changes),
            Source::Staged => fresh(&self.changes.staged, &staged),
        };
        let any_changed =
            fresh(&self.changes.changes, &changes) || fresh(&self.changes.staged, &staged);
        self.changes.changes = changes;
        self.changes.staged = staged;
        // An emptied comparison gives way to the other, so the diff is never blank needlessly.
        let other = match self.changes.source {
            Source::Changes => Source::Staged,
            Source::Staged => Source::Changes,
        };
        if self.changes.loaded(self.changes.source).files.is_empty()
            && !self.changes.loaded(other).files.is_empty()
        {
            self.changes.source = other;
            self.show_source(cx);
        } else if shown_changed || self.changes.diff.is_none() {
            self.show_source(cx);
        }
        if any_changed {
            cx.notify();
        }
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
        let patch = |text: &str| git::PatchText {
            text: text.to_owned(),
            reduced: false,
        };
        let snapshot = Snapshot {
            changes: review::parse(patch(changes), &[]),
            staged: review::parse(patch(staged), &[]),
            status: status.clone(),
        };
        self.set_changes_view(View::Repo(status), Some(snapshot), cx);
    }
}

/// Status, then both comparisons, then the text of each conflicted file.
fn read_checkout(folder: &std::path::Path) -> Result<Option<Snapshot>, String> {
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
    Ok(Some(Snapshot {
        changes: review::parse(changes, &conflicts),
        staged: review::parse(staged, &[]),
        status,
    }))
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
