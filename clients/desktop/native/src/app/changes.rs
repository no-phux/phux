//! The Changes panel's state: which checkout it follows, its last status, the
//! file whose diff is open, and the commit message.
//!
//! The panel follows the focused pane's working directory, so an agent's
//! edits show beside the pane it runs in. Paths resolve only on this Mac: a
//! remote window or a satellite terminal shows why instead of reading some
//! other local folder. Every `git` call runs off the UI thread; a result for a
//! folder the panel has since left is dropped. While open, the panel re-reads
//! the status on a short poll and repaints only when something changed.

use std::path::PathBuf;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{Context, Entity, Focusable, ScrollHandle, Subscription, Task, Window};
use gpui_kit::component::input::{InputEvent, TextareaState};

use crate::connection::path as paths;
use crate::git::{self, Area, Diff, Entry, Op, Status};

use super::desk::Desk;
use super::notifications::Kind;

/// How often an open panel re-reads the status.
const POLL: Duration = Duration::from_secs(3);

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

/// The file whose diff is open under its row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selected {
    pub entry: Entry,
    /// `None` while it loads.
    pub diff: Option<Result<Diff, String>>,
}

#[derive(Default)]
pub(crate) struct Changes {
    pub open: bool,
    pub view: View,
    /// The folder the view was read for.
    folder: Option<PathBuf>,
    /// Bumped per read; a result carrying an older number is stale.
    generation: u64,
    reading: bool,
    pub selected: Option<Selected>,
    /// An operation (stage, unstage, commit) is running.
    pub busy: bool,
    /// A commit landed; the field empties when next prepared.
    pending_clear: bool,
    pub message: Option<(Entity<TextareaState>, Subscription)>,
    pub scroll: ScrollHandle,
    _poll: Option<Task<()>>,
}

impl Changes {
    /// Whether the commit field holds the keyboard, so the terminal must not take it.
    pub(crate) fn has_keys(&self, window: &Window, cx: &gpui::App) -> bool {
        self.open
            && self
                .message
                .as_ref()
                .is_some_and(|(input, _)| input.read(cx).focus_handle(cx).is_focused(window))
    }

    pub(crate) fn status(&self) -> Option<&Status> {
        match &self.view {
            View::Repo(status) => Some(status),
            _ => None,
        }
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
            self.changes.selected = None;
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
    pub(crate) fn follow_changes(&mut self, cx: &mut Context<Self>) {
        if self.changes.open && self.changes_folder().ok() != self.changes.folder {
            self.refresh_changes(true, cx);
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

    /// Re-read the status for the focused pane's folder. `force` reads even
    /// while another read is in flight, such as after the focus moved.
    pub(crate) fn refresh_changes(&mut self, force: bool, cx: &mut Context<Self>) {
        if !self.changes.open || (self.changes.reading && !force) {
            return;
        }
        let folder = match self.changes_folder() {
            Ok(folder) => folder,
            Err(reason) => {
                self.changes.folder = None;
                self.changes.selected = None;
                self.set_changes_view(View::Unavailable(reason), cx);
                return;
            }
        };
        if self.changes.folder.as_ref() != Some(&folder) {
            self.changes.selected = None;
            self.changes.folder = Some(folder.clone());
            self.set_changes_view(View::Loading, cx);
        }
        self.changes.generation += 1;
        self.changes.reading = true;
        let generation = self.changes.generation;
        cx.spawn(async move |this, cx| {
            let read = folder.clone();
            let result = smol::unblock(move || git::read_status(&read)).await;
            let _ = this.update(cx, |desk, cx| {
                if desk.changes.generation != generation {
                    return;
                }
                desk.changes.reading = false;
                let view = match result {
                    Ok(Some(status)) => View::Repo(status),
                    Ok(None) => View::NotRepo(folder),
                    Err(error) => View::Failed(error),
                };
                desk.set_changes_view(view, cx);
            });
        })
        .detach();
    }

    /// Repaint only for a different view. A selected file that left the list
    /// closes; one still listed reloads its diff, whose text can change while
    /// its status stays the same.
    fn set_changes_view(&mut self, view: View, cx: &mut Context<Self>) {
        if self.changes.view != view {
            self.changes.view = view;
            cx.notify();
        }
        let Some(selected) = self.changes.selected.clone() else {
            return;
        };
        let listed = self
            .changes
            .status()
            .is_some_and(|status| status.entries.contains(&selected.entry));
        if listed {
            self.load_diff(selected.entry, cx);
        } else {
            self.changes.selected = None;
            cx.notify();
        }
    }

    /// Open `entry`'s diff, or close it when it is already open.
    pub(crate) fn select_change(&mut self, entry: Entry, cx: &mut Context<Self>) {
        if self
            .changes
            .selected
            .as_ref()
            .is_some_and(|selected| selected.entry == entry)
        {
            self.changes.selected = None;
            cx.notify();
            return;
        }
        self.changes.selected = Some(Selected {
            entry: entry.clone(),
            diff: None,
        });
        self.load_diff(entry, cx);
        cx.notify();
    }

    fn load_diff(&mut self, entry: Entry, cx: &mut Context<Self>) {
        let Some(root) = self.changes.status().map(|status| status.root.clone()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let wanted = entry.clone();
            let diff = smol::unblock(move || git::read_diff(&root, &wanted)).await;
            let _ = this.update(cx, |desk, cx| {
                if let Some(selected) = desk
                    .changes
                    .selected
                    .as_mut()
                    .filter(|selected| selected.entry == entry)
                    && selected.diff.as_ref() != Some(&diff)
                {
                    selected.diff = Some(diff);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Run `op` against the shown checkout, then re-read it. A commit that
    /// lands clears the message; a refusal (a hook, nothing staged) posts
    /// git's own words and keeps the message.
    pub(crate) fn change_index(&mut self, op: Op, cx: &mut Context<Self>) {
        if self.changes.busy {
            return;
        }
        let Some(root) = self.changes.status().map(|status| status.root.clone()) else {
            return;
        };
        self.changes.busy = true;
        cx.notify();
        let committing = matches!(op, Op::Commit(_));
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || git::apply(&root, &op)).await;
            let _ = this.update(cx, |desk, cx| {
                desk.changes.busy = false;
                match result {
                    Ok(()) if committing => {
                        desk.changes.pending_clear = true;
                        desk.post(Kind::Info, "Committed", None);
                    }
                    Ok(()) => {}
                    Err(error) => {
                        let title = if committing {
                            "Commit refused"
                        } else {
                            "Git refused the change"
                        };
                        desk.post(Kind::Error, title, Some(error));
                    }
                }
                desk.refresh_changes(true, cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn stage_change(&mut self, entry: &Entry, cx: &mut Context<Self>) {
        let op = match entry.area {
            Area::Staged => Op::Unstage(
                std::iter::once(entry.path.clone())
                    .chain(entry.from.clone())
                    .collect(),
            ),
            Area::Unstaged | Area::Untracked | Area::Conflicted => {
                Op::Stage(vec![entry.path.clone()])
            }
        };
        self.change_index(op, cx);
    }

    pub(crate) fn commit_changes(&mut self, cx: &mut Context<Self>) {
        if !self.changes.can_commit(cx) {
            return;
        }
        let message = self.changes.message_text(cx).trim().to_owned();
        self.change_index(Op::Commit(message), cx);
    }

    /// Build the commit field once a window is at hand, and empty it after a
    /// commit landed.
    pub(crate) fn prepare_changes_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.changes.message.is_none() {
            let input = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .placeholder("Commit message")
                    .auto_grow(1, 6)
                    .submit_on_enter(true)
            });
            // Enter commits and Shift+Enter adds a line, as in the conversation composer.
            let subscription = cx.subscribe_in(
                &input,
                window,
                |desk, _, event: &InputEvent, _, cx| match event {
                    InputEvent::PressEnter { shift: false, .. } => desk.commit_changes(cx),
                    InputEvent::Change => cx.notify(),
                    _ => {}
                },
            );
            self.changes.message = Some((input, subscription));
        }
        if std::mem::take(&mut self.changes.pending_clear)
            && let Some((input, _)) = self.changes.message.as_ref()
        {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
    }
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
}
