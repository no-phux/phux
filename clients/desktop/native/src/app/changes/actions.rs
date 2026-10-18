//! What the Changes panel does to the checkout and the agent: stage and
//! unstage files or selected lines, accept a conflict's resolution, commit,
//! and write, keep and send review comments.

use gpui::prelude::*;
use gpui::{Context, Entity, Subscription, Window};
use gpui_kit::component::diff::DiffLineRange;
use gpui_kit::component::input::{InputEvent, TextareaState};

use crate::git::{Area, Op, Source};

use super::super::desk::Desk;
use super::super::notifications::Kind;
use super::review::{self, Comment, Draft};

impl Desk {
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
            let result = smol::unblock(move || crate::git::apply(&root, &op)).await;
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

    /// Stage `path` out of Changes, or unstage it out of Staged. A
    /// conflicted file is staged with its resolution, once every conflict has one.
    pub(crate) fn stage_path(&mut self, source: Source, path: &str, cx: &mut Context<Self>) {
        let Some(entry) = self.changes.status().and_then(|status| {
            status.entries.iter().find(|entry| {
                entry.path == path && (entry.area == Area::Staged) == (source == Source::Staged)
            })
        }) else {
            return;
        };
        let op = match entry.area {
            Area::Staged => Op::Unstage(
                std::iter::once(entry.path.clone())
                    .chain(entry.from.clone())
                    .collect(),
            ),
            Area::Conflicted => {
                let resolved = self
                    .changes
                    .diff
                    .as_ref()
                    .and_then(|(state, _)| state.read(cx).resolved_text(path));
                let Some(text) = resolved else {
                    self.post(
                        Kind::Info,
                        "Resolve every conflict first",
                        Some("Pick a side for each conflict in the diff, then mark the file resolved.".into()),
                    );
                    return;
                };
                Op::Resolve {
                    path: path.to_owned(),
                    text,
                }
            }
            Area::Unstaged | Area::Untracked => Op::Stage(vec![entry.path.clone()]),
        };
        self.change_index(op, cx);
    }

    /// Stage the selected lines out of Changes, or unstage them out of Staged.
    pub(crate) fn stage_selected_lines(&mut self, cx: &mut Context<Self>) {
        let Some((state, _)) = self.changes.diff.as_ref() else {
            return;
        };
        let Some(range) = state.read(cx).selected_lines() else {
            return;
        };
        let reverse = self.changes.source == Source::Staged;
        let section = self
            .changes
            .loaded(self.changes.source)
            .sections
            .get(range.path().as_ref());
        let cut = section
            .and_then(|file| crate::git::partial_patch(file, review::selection(&range), reverse));
        let Some(patch) = cut else {
            self.post(
                Kind::Info,
                "No changed lines selected",
                Some("Select added or removed lines to stage them.".into()),
            );
            return;
        };
        state.update(cx, |state, cx| state.set_selected_lines(None, cx));
        self.change_index(Op::Apply { patch, reverse }, cx);
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
            let field = composer("Commit message", 6, Self::commit_changes, window, cx);
            self.changes.message = Some(field);
        }
        if std::mem::take(&mut self.changes.pending_clear)
            && let Some((input, _)) = self.changes.message.as_ref()
        {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    /// Open a note field under `range`, replacing any unsaved one.
    pub(crate) fn start_comment(
        &mut self,
        range: DiffLineRange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (input, subscription) =
            composer("Note for the agent", 8, Self::save_comment, window, cx);
        input.update(cx, |input, cx| input.focus(window, cx));
        self.changes.draft = Some(Draft {
            source: self.changes.source,
            range,
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Keep the comment being written; an empty one is dropped.
    pub(crate) fn save_comment(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.changes.draft.take() else {
            return;
        };
        let body = draft.input.read(cx).value().trim().to_owned();
        if !body.is_empty() {
            self.changes.next_comment += 1;
            self.changes.comments.push(Comment {
                id: self.changes.next_comment,
                source: draft.source,
                range: draft.range,
                body,
            });
        }
        cx.notify();
    }

    pub(crate) fn cancel_comment(&mut self, cx: &mut Context<Self>) {
        if self.changes.draft.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn remove_comment(&mut self, id: u64, cx: &mut Context<Self>) {
        self.changes.comments.retain(|comment| comment.id != id);
        cx.notify();
    }

    /// Paste every comment, as one numbered review, into the terminal the
    /// panel follows. Nothing is submitted: the reviewer presses Enter there.
    pub(crate) fn send_review(&mut self, cx: &mut Context<Self>) {
        if self.changes.comments.is_empty() {
            return;
        }
        let Some(terminal) = self.changes.terminal.clone() else {
            self.post(
                Kind::Error,
                "No terminal to send to",
                Some("Focus the agent's terminal, then send the review.".into()),
            );
            return;
        };
        let repo = self
            .changes
            .status()
            .map_or_else(|| "this checkout".to_owned(), crate::git::Status::name);
        let prompt = review::review_prompt(&repo, &self.changes.comments);
        if self.paste_text_to(&terminal, &prompt) {
            let count = self.changes.comments.len();
            self.changes.comments.clear();
            let title = self.pane_title(Some(&terminal));
            let notes = if count == 1 {
                "1 note".to_owned()
            } else {
                format!("{count} notes")
            };
            self.post(
                Kind::Info,
                "Review sent",
                Some(format!(
                    "{notes} pasted into {title}. Press Enter there to send them."
                )),
            );
        }
        cx.notify();
    }
}

/// A growing text field where Enter runs `submit` and Shift+Enter adds a
/// line, as in the conversation composer.
fn composer(
    placeholder: &'static str,
    rows: usize,
    submit: fn(&mut Desk, &mut Context<Desk>),
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> (Entity<TextareaState>, Subscription) {
    let input = cx.new(|cx| {
        TextareaState::new(window, cx)
            .placeholder(placeholder)
            .auto_grow(1, rows)
            .submit_on_enter(true)
    });
    let subscription = cx.subscribe_in(
        &input,
        window,
        move |desk, _, event: &InputEvent, _, cx| match event {
            InputEvent::PressEnter { shift: false, .. } => submit(desk, cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        },
    );
    (input, subscription)
}
