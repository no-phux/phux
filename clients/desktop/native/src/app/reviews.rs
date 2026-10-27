//! The Reviews surface's state: the inbox of open pull requests that want
//! you, and the one opened in full.
//!
//! GitHub is read through the user's own `gh`, so it signs in, picks hosts and
//! honors Enterprise exactly as in a terminal. Every call runs off the UI
//! thread; a result for an inbox or pull request the surface has since left
//! is dropped by its generation. While open, the inbox and the opened pull
//! request re-read on a poll and repaint only when something changed.
//!
//! | Module | Owns |
//! |---|---|
//! | [`local`] | which checkout on this Mac holds a repository, and who works on a branch |
//! | [`load`] | reading the inbox, a pull request, its patch and a job's log |
//! | [`actions`] | comments, replies, resolution, viewed marks |
//! | [`lines`] | a comment's lines in GitHub's coordinates |
//! | [`submit`] | the review, to GitHub and to the branch's agent |
//! | [`merge`] | merge, auto-merge, ready for review, check out |
//! | [`model`] | a pull request's key, what was read for it, the review on it |
//! | [`keys`] | the surface's own keys and where the keyboard goes after |
//! | [`prompt`] | the review as a message for the agent on the branch |

mod actions;
#[cfg(any(test, feature = "ui-fixtures"))]
mod fixture;
mod keys;
mod lines;
mod load;
pub(crate) mod local;
mod merge;
mod model;
pub(crate) use model::*;
pub(crate) mod prompt;
mod submit;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::time::Duration;

use gpui::{Context, Entity, FocusHandle, Focusable, Task, Window};
use gpui_kit::component::input::TextareaState;

use crate::git::github::{Inbox, InboxPull, Pull, ReviewEvent};

use super::desk::Desk;

/// How often an open surface re-reads the inbox.
const INBOX_POLL: Duration = Duration::from_secs(90);
/// How often an open pull request re-reads; a push reloads its diff.
const PULL_POLL: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(crate) struct Reviews {
    pub open: bool,
    /// How the diff lays out its two sides.
    pub layout: super::changes::Layout,
    pub focus: Option<FocusHandle>,
    /// The viewer, then every account they administer.
    owners: Option<Vec<String>>,
    pub inbox: Fetched<Inbox>,
    pub filter: Filter,
    /// The inbox row the keyboard is on.
    pub cursor: Option<Key>,
    pub opened: Option<Opened>,
    /// Unsent reviews of pull requests not open now.
    pub kept: HashMap<Key, Kept>,
    /// Fixture data: nothing is read from GitHub.
    pub offline: bool,
    generation: u64,
    _poll: Option<Task<()>>,
}

impl Reviews {
    /// The inbox rows the filter admits, newest activity first.
    pub(crate) fn shown(&self) -> Vec<&InboxPull> {
        let Some(inbox) = &self.inbox.data else {
            return Vec::new();
        };
        inbox
            .pulls
            .iter()
            .filter(|pull| self.filter.admits(pull))
            .collect()
    }

    /// The listed pull requests grouped by repository, the most recently
    /// active repository first; each group keeps its newest-first order.
    pub(crate) fn groups(&self) -> Vec<(String, Vec<&InboxPull>)> {
        let mut groups: Vec<(String, Vec<&InboxPull>)> = Vec::new();
        for pull in self.shown() {
            match groups.iter_mut().find(|(repo, _)| *repo == pull.repo) {
                Some((_, pulls)) => pulls.push(pull),
                None => groups.push((pull.repo.clone(), vec![pull])),
            }
        }
        groups
    }

    /// The inbox in the order the list draws it.
    pub(crate) fn ordered(&self) -> Vec<Key> {
        self.groups()
            .into_iter()
            .flat_map(|(_, pulls)| pulls.into_iter().map(Key::of))
            .collect()
    }

    pub(crate) fn listed(&self, key: &Key) -> Option<&InboxPull> {
        self.inbox
            .data
            .as_ref()?
            .pulls
            .iter()
            .find(|pull| pull.repo == key.repo && pull.number == key.number)
    }

    pub(crate) fn opened_key(&self) -> Option<&Key> {
        self.opened.as_ref().map(|opened| &opened.key)
    }

    /// Whether a Reviews text field or the diff holds the keyboard.
    pub(crate) fn has_keys(&self, window: &Window, cx: &gpui::App) -> bool {
        let Some(opened) = self.opened.as_ref().filter(|_| self.open) else {
            return false;
        };
        let focused =
            |input: &Entity<TextareaState>| input.read(cx).focus_handle(cx).is_focused(window);
        opened
            .draft
            .as_ref()
            .is_some_and(|draft| focused(&draft.input))
            || opened
                .summary
                .as_ref()
                .is_some_and(|(input, _)| focused(input))
    }

    /// Whether the diff view holds the keyboard (its own arrows and copy).
    pub(crate) fn diff_has_keys(&self, window: &Window, cx: &gpui::App) -> bool {
        self.opened
            .as_ref()
            .and_then(|opened| opened.diff.as_ref())
            .is_some_and(|(state, _)| state.focus_handle(cx).contains_focused(window, cx))
    }
}

/// The local branch a pull request checks out as: its own name, or
/// `pr-N` for a fork's, whose name could clash with a branch here.
pub(crate) fn checkout_branch(pull: &Pull) -> String {
    if pull.is_fork() {
        format!("pr-{}", pull.number)
    } else {
        pull.head_ref.clone()
    }
}

/// Unix milliseconds of GitHub's `2026-10-23T16:25:00Z`.
pub(crate) fn iso_ms(at: &str) -> Option<u64> {
    let (date, time) = at.split_once('T')?;
    let mut date = date.splitn(3, '-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let time = time.trim_end_matches('Z');
    let time = time.split(['.', '+']).next()?;
    let mut time = time.splitn(3, ':').map(str::parse::<i64>);
    let (hour, minute, second) = (time.next()?.ok()?, time.next()?.ok()?, time.next()?.ok()?);
    // Days from the civil date (Howard Hinnant's algorithm).
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let of_cycle = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    let days = era * 146_097 + of_cycle - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second;
    u64::try_from(seconds).ok().map(|seconds| seconds * 1000)
}

/// The narrowest diff an automatic layout splits: a review's diff holds
/// threads and long lines, so it splits later than the Changes panel.
const SPLIT_WIDTH: f32 = 1100.;

/// The diff's mode for `layout` at `width` points.
pub(crate) fn diff_mode(
    layout: super::changes::Layout,
    width: f32,
) -> gpui_kit::component::diff::DiffMode {
    use super::changes::Layout;
    use gpui_kit::component::diff::DiffMode;
    match layout {
        Layout::Split => DiffMode::Split,
        Layout::Unified => DiffMode::Unified,
        Layout::Auto if width >= SPLIT_WIDTH => DiffMode::Split,
        Layout::Auto => DiffMode::Unified,
    }
}

/// Commands that act on the panes close Reviews first, so their effect is seen.
pub(crate) fn hides_reviews(id: &str) -> bool {
    !matches!(
        id,
        "reviews"
            | "palette"
            | "goto"
            | "notifications"
            | "capture"
            | "settings"
            | "main-menu"
            | "fullscreen"
            | "minimize"
            | "maximize"
            | "new-window"
            | "quick-terminal"
            | "reconnect"
            | "reload-config"
            | "copy-diagnostics"
            | "developer-mode"
            | "perf-hud"
            | "perf-trace"
            | "performance-monitor"
            | "performance-monitor-stop"
            | "clear-notifications"
    )
}

impl Desk {
    pub(crate) fn toggle_reviews(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The quick terminal has no room for it and never draws it.
        if self.role == super::desk::WindowRole::Quick {
            return;
        }
        if self.reviews.open {
            self.close_reviews(window, cx);
        } else {
            self.open_reviews(window, cx);
        }
    }

    pub(crate) fn open_reviews(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reviews.open = true;
        let focus = self
            .reviews
            .focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        window.focus(&focus, cx);
        self.refresh_inbox(cx);
        if let Some(key) = self.reviews.opened_key().cloned() {
            self.read_pull(&key, cx);
        }
        self.arm_reviews_poll(cx);
        cx.notify();
    }

    pub(crate) fn close_reviews(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.reviews.open {
            return;
        }
        self.reviews.open = false;
        self.reviews._poll = None;
        self.focus_terminal(window, cx);
        cx.notify();
    }

    fn arm_reviews_poll(&mut self, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        self.reviews._poll = Some(cx.spawn(async move |this, cx| {
            let mut since_inbox = Duration::ZERO;
            loop {
                executor.timer(PULL_POLL).await;
                since_inbox += PULL_POLL;
                let Ok(open) = this.update(cx, |desk, cx| {
                    if !desk.reviews.open {
                        return false;
                    }
                    if since_inbox >= INBOX_POLL {
                        desk.refresh_inbox(cx);
                    }
                    if let Some(key) = desk.reviews.opened_key().cloned() {
                        desk.read_pull(&key, cx);
                    }
                    true
                }) else {
                    return;
                };
                if since_inbox >= INBOX_POLL {
                    since_inbox = Duration::ZERO;
                }
                if !open {
                    return;
                }
            }
        }));
    }

    pub(crate) fn set_reviews_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        self.reviews.filter = filter;
        cx.notify();
    }

    /// Move the inbox cursor `step` rows and open what it lands on.
    pub(crate) fn step_review(&mut self, step: isize, cx: &mut Context<Self>) {
        let order = self.reviews.ordered();
        if order.is_empty() {
            return;
        }
        let at = self
            .reviews
            .cursor
            .as_ref()
            .or(self.reviews.opened_key())
            .and_then(|key| order.iter().position(|each| each == key));
        let next = match at {
            Some(at) => at.saturating_add_signed(step).min(order.len() - 1),
            None if step < 0 => order.len() - 1,
            None => 0,
        };
        self.open_pull(order[next].clone(), cx);
    }

    /// Show `key` in full. The unsent review of the one it replaces is kept,
    /// and `key`'s own comes back.
    pub(crate) fn open_pull(&mut self, key: Key, cx: &mut Context<Self>) {
        self.reviews.cursor = Some(key.clone());
        if self.reviews.opened_key() == Some(&key) {
            cx.notify();
            return;
        }
        if let Some(previous) = self.reviews.opened.take() {
            let summary = previous.summary_text(cx);
            if !previous.pending.is_empty()
                || !summary.is_empty()
                || previous.event != ReviewEvent::Comment
            {
                let kept = Kept {
                    pending: previous.pending,
                    next_pending: previous.next_pending,
                    summary,
                    event: previous.event,
                    to_agent: previous.to_agent,
                };
                self.reviews.kept.insert(previous.key, kept);
            }
        }
        self.reviews.generation += 1;
        let mut opened = Opened::new(key.clone(), self.reviews.generation);
        if let Some(kept) = self.reviews.kept.remove(&key) {
            opened.pending = kept.pending;
            opened.next_pending = kept.next_pending;
            opened.kept_summary = kept.summary;
            opened.event = kept.event;
            opened.to_agent = kept.to_agent;
        }
        self.reviews.opened = Some(opened);
        self.read_pull(&key, cx);
        cx.notify();
    }

    pub(crate) fn set_review_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if let Some(opened) = self.reviews.opened.as_mut() {
            opened.tab = tab;
            cx.notify();
        }
    }

    pub(crate) fn open_pull_in_browser(&mut self, cx: &mut Context<Self>) {
        let url = self
            .reviews
            .opened
            .as_ref()
            .and_then(|opened| opened.pull.data.as_ref().map(|pull| pull.url.clone()))
            .or_else(|| {
                let key = self.reviews.cursor.as_ref()?;
                Some(self.reviews.listed(key)?.url.clone())
            });
        if let Some(url) = url.filter(|url| url.starts_with("https://")) {
            cx.open_url(&url);
        }
    }
}
