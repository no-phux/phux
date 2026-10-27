//! What a reviewer does to a pull request as they read it: line comments
//! kept for one review, replies and resolution posted at once, viewed marks,
//! and the review bar's fields.

use gpui::prelude::*;
use gpui::{Context, Entity, Subscription, Window};
use gpui_kit::component::diff::DiffLineRange;
use gpui_kit::component::input::{InputEvent, TextareaState};

use crate::git::github::{self, ReviewEvent, write};

use super::super::Kind;
use super::super::desk::Desk;
use super::{Draft, DraftFor, Pending, Tab};

impl Desk {
    /// Run `write` off the UI thread with the opened pull request marked busy
    /// as `what`, then `done` with its result and whether that pull request
    /// is still the one open, and reread it. `done` always runs, so an
    /// outcome is reported even after the reviewer moved on.
    pub(super) fn review_write<T: Send + 'static>(
        &mut self,
        what: &'static str,
        write: impl FnOnce() -> github::gh::Result<T> + Send + 'static,
        done: impl FnOnce(&mut Self, Result<T, String>, bool, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(opened) = self.reviews.opened.as_mut() else {
            return;
        };
        if let Some(running) = opened.busy {
            self.post(
                Kind::Refusal,
                "Still working",
                Some(format!("{running}… finishes first.")),
            );
            cx.notify();
            return;
        }
        opened.busy = Some(what);
        let generation = opened.generation;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || write().map_err(|error| error.to_string())).await;
            let _ = this.update(cx, |desk, cx| {
                let current = desk
                    .reviews
                    .opened
                    .as_mut()
                    .filter(|o| o.generation == generation);
                let key = current.map(|opened| {
                    opened.busy = None;
                    opened.key.clone()
                });
                done(desk, result, key.is_some(), cx);
                if let Some(key) = key {
                    desk.read_pull(&key, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Open a comment field under `range`, replacing any unsaved one.
    pub(crate) fn start_review_comment(
        &mut self,
        range: DiffLineRange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_review_draft(DraftFor::Lines(range), "Comment for the review", window, cx);
    }

    /// Open a reply field on `thread`.
    pub(crate) fn start_reply(
        &mut self,
        thread: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_review_draft(DraftFor::Reply(thread), "Reply", window, cx);
    }

    fn start_review_draft(
        &mut self,
        target: DraftFor,
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reviews.opened.is_none() {
            return;
        }
        let (input, subscription) =
            composer(placeholder, 10, Some(Self::save_review_draft), window, cx);
        input.update(cx, |input, cx| input.focus(window, cx));
        if let Some(opened) = self.reviews.opened.as_mut() {
            opened.draft = Some(Draft {
                target,
                input,
                _subscription: subscription,
            });
        }
        cx.notify();
    }

    /// Keep a line comment for the review, or post a reply now. A reply's
    /// field stays until GitHub takes it, so a refusal loses nothing.
    pub(crate) fn save_review_draft(&mut self, cx: &mut Context<Self>) {
        let Some(opened) = self.reviews.opened.as_mut() else {
            return;
        };
        let Some(draft) = opened.draft.as_ref() else {
            return;
        };
        let body = draft.input.read(cx).value().trim().to_owned();
        if body.is_empty() {
            opened.draft = None;
            cx.notify();
            return;
        }
        match draft.target.clone() {
            DraftFor::Lines(range) => {
                opened.draft = None;
                opened.next_pending += 1;
                let id = opened.next_pending;
                opened.pending.push(Pending { id, range, body });
                cx.notify();
            }
            DraftFor::Reply(thread) => {
                let replied = DraftFor::Reply(thread.clone());
                self.review_write(
                    "Replying",
                    move || write::reply(&thread, &body),
                    move |desk, result, current, _| match result {
                        Ok(()) if current => {
                            let opened = desk.reviews.opened.as_mut();
                            if let Some(opened) = opened.filter(|o| {
                                o.draft
                                    .as_ref()
                                    .is_some_and(|draft| draft.target == replied)
                            }) {
                                opened.draft = None;
                            }
                        }
                        Ok(()) => {}
                        Err(error) => desk.post(Kind::Error, "Could not reply", Some(error)),
                    },
                    cx,
                );
            }
        }
    }

    pub(crate) fn cancel_review_draft(&mut self, cx: &mut Context<Self>) {
        if let Some(opened) = self.reviews.opened.as_mut()
            && opened.draft.take().is_some()
        {
            cx.notify();
        }
    }

    pub(crate) fn remove_pending(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(opened) = self.reviews.opened.as_mut() {
            opened.pending.retain(|pending| pending.id != id);
            cx.notify();
        }
    }

    pub(crate) fn set_thread_resolved(
        &mut self,
        thread: String,
        resolved: bool,
        cx: &mut Context<Self>,
    ) {
        let what = if resolved { "Resolving" } else { "Reopening" };
        self.review_write(
            what,
            move || write::set_resolved(&thread, resolved),
            |desk, result, _, _| {
                if let Err(error) = result {
                    desk.post(Kind::Error, "Could not change the thread", Some(error));
                }
            },
            cx,
        );
    }

    pub(crate) fn toggle_thread_expanded(&mut self, thread: &str, cx: &mut Context<Self>) {
        if let Some(opened) = self.reviews.opened.as_mut() {
            if !opened.expanded.remove(thread) {
                opened.expanded.insert(thread.to_owned());
            }
            cx.notify();
        }
    }

    /// Tick or clear `path`'s viewed box here and on GitHub; a refusal undoes
    /// it, unless the reviewer changed it again meanwhile.
    pub(crate) fn toggle_review_viewed(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(opened) = self.reviews.opened.as_mut() else {
            return;
        };
        let Some(pull_id) = opened.pull.data.as_ref().map(|pull| pull.id.clone()) else {
            return;
        };
        let viewed = !opened.is_viewed(path);
        opened.viewed.insert(path.to_owned(), viewed);
        let generation = opened.generation;
        self.sync_review_diff(cx);
        cx.notify();
        let path = path.to_owned();
        cx.spawn(async move |this, cx| {
            let sent = path.clone();
            let result = smol::unblock(move || write::set_viewed(&pull_id, &sent, viewed)).await;
            let _ = this.update(cx, |desk, cx| {
                let Err(error) = result else { return };
                if let Some(opened) = desk
                    .reviews
                    .opened
                    .as_mut()
                    .filter(|o| o.generation == generation)
                    && opened.viewed.get(&path) == Some(&viewed)
                {
                    opened.viewed.insert(path, !viewed);
                }
                desk.sync_review_diff(cx);
                desk.post(
                    Kind::Error,
                    "Could not mark the file",
                    Some(error.to_string()),
                );
                cx.notify();
            });
        })
        .detach();
    }

    /// Scroll the diff to `path`, from the file list.
    pub(crate) fn reveal_review_file(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(opened) = self.reviews.opened.as_mut() else {
            return;
        };
        opened.focus = Some(path.to_owned());
        opened.tab = Tab::Files;
        if let Some((state, _)) = &opened.diff {
            state.update(cx, |state, cx| state.scroll_to_file(path, cx));
        }
        cx.notify();
    }

    pub(crate) fn set_review_event(&mut self, event: ReviewEvent, cx: &mut Context<Self>) {
        if let Some(opened) = self.reviews.opened.as_mut() {
            opened.event = event;
            cx.notify();
        }
    }

    pub(crate) fn toggle_review_to_agent(&mut self, cx: &mut Context<Self>) {
        if let Some(opened) = self.reviews.opened.as_mut() {
            opened.to_agent = !opened.to_agent;
            cx.notify();
        }
    }

    /// Give the summary field the keyboard.
    pub(crate) fn focus_review_summary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_review_summary(window, cx);
        let input = self
            .reviews
            .opened
            .as_ref()
            .and_then(|opened| opened.summary.as_ref())
            .map(|(input, _)| input.clone());
        if let Some(input) = input {
            input.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    /// The summary field, made on first draw so the review bar always has
    /// one, holding the text kept from when this pull request was last open.
    pub(crate) fn ensure_review_summary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(opened) = self.reviews.opened.as_mut() else {
            return;
        };
        if opened.summary.is_some() {
            return;
        }
        let kept = std::mem::take(&mut opened.kept_summary);
        let (input, subscription) = composer("Leave a summary", 6, None, window, cx);
        if !kept.is_empty() {
            input.update(cx, |input, cx| input.set_value(kept, window, cx));
        }
        if let Some(opened) = self.reviews.opened.as_mut() {
            opened.summary = Some((input, subscription));
        }
    }
}

/// A growing text field. With `submit`, Enter runs it and Shift+Enter adds a
/// line; without, Enter adds a line (a review summary is prose).
pub(super) fn composer(
    placeholder: &'static str,
    rows: usize,
    submit: Option<fn(&mut Desk, &mut Context<Desk>)>,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> (Entity<TextareaState>, Subscription) {
    let input = cx.new(|cx| {
        TextareaState::new(window, cx)
            .placeholder(placeholder)
            .auto_grow(1, rows)
            .submit_on_enter(submit.is_some())
    });
    let subscription = cx.subscribe_in(
        &input,
        window,
        move |desk, _, event: &InputEvent, _, cx| match (event, submit) {
            (InputEvent::PressEnter { shift: false, .. }, Some(submit)) => submit(desk, cx),
            (InputEvent::Change, _) => cx.notify(),
            _ => {}
        },
    );
    (input, subscription)
}
