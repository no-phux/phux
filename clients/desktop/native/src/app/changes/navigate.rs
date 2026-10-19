//! Moving around the review: which comparison the diff shows, which file it
//! scrolls to, viewed marks and folded folders.

use gpui::Context;
use gpui::prelude::*;
use gpui_kit::component::diff::{DiffEvent, DiffInlineUnit, DiffState};

use crate::git::Source;

use super::super::desk::Desk;
use super::review;

impl Desk {
    /// Put the shown comparison's files in the diff, folding viewed ones, and
    /// return to the file last revealed.
    pub(crate) fn show_source(&mut self, cx: &mut Context<Self>) {
        let files = self.changes.loaded(self.changes.source).files.clone();
        let viewed: Vec<String> = files
            .iter()
            .map(|file| file.path().to_string())
            .filter(|path| self.changes.is_viewed(path))
            .collect();
        let focus = self.changes.focus.clone();
        let state = match &self.changes.diff {
            Some((state, _)) => {
                state.update(cx, |state, cx| state.set_files(files, cx));
                state.clone()
            }
            None => {
                let state = cx.new(|cx| {
                    DiffState::new(files, cx)
                        .with_context_lines(Some(review::SHOWN_CONTEXT))
                        .with_inline_unit(Some(DiffInlineUnit::Word))
                });
                // The toolbar's line staging and a conflicted file's
                // resolve button follow these.
                let subscription = cx.subscribe(&state, |_, _, event: &DiffEvent, cx| {
                    if matches!(
                        event,
                        DiffEvent::SelectionChanged(_)
                            | DiffEvent::SelectionEnded(_)
                            | DiffEvent::ConflictResolved(..)
                    ) {
                        cx.notify();
                    }
                });
                self.changes.diff = Some((state.clone(), subscription));
                state
            }
        };
        // Folds outlive a reload, so a file the agent changed after it was
        // viewed unfolds again as its mark lapses.
        let lapsed = self.changes.lapse_viewed();
        state.update(cx, |state, cx| {
            for path in &lapsed {
                state.set_file_collapsed(path, false, cx);
            }
            for path in &viewed {
                state.set_file_collapsed(path, true, cx);
            }
            if let Some(path) = focus {
                state.scroll_to_file(&path, cx);
            }
        });
        cx.notify();
    }

    /// Show `source` and scroll to `path` in it.
    pub(crate) fn reveal_change(&mut self, source: Source, path: &str, cx: &mut Context<Self>) {
        self.changes.focus = Some(path.to_owned());
        if self.changes.source != source {
            self.changes.source = source;
            self.show_source(cx);
        } else if let Some((state, _)) = &self.changes.diff {
            state.update(cx, |state, cx| {
                state.set_file_collapsed(path, false, cx);
                state.scroll_to_file(path, cx);
            });
        }
        cx.notify();
    }

    pub(crate) fn set_change_source(&mut self, source: Source, cx: &mut Context<Self>) {
        if self.changes.source != source {
            self.changes.source = source;
            self.changes.focus = None;
            self.show_source(cx);
        }
    }

    /// Mark `path` viewed, folding it, or unmark it.
    pub(crate) fn toggle_viewed(&mut self, path: &str, cx: &mut Context<Self>) {
        let viewed = !self.changes.is_viewed(path);
        self.changes.set_viewed(path, viewed);
        if let Some((state, _)) = &self.changes.diff {
            state.update(cx, |state, cx| state.set_file_collapsed(path, viewed, cx));
        }
        cx.notify();
    }

    pub(crate) fn toggle_folder(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.changes.collapsed.remove(key) {
            self.changes.collapsed.insert(key.to_owned());
        }
        cx.notify();
    }
}
