//! Agent badges beside the canvas (design spec §1.3, §4.4): colour, mark
//! and word together, from the one status mapping, `phux_design::tone_of`.
//!
//! Each badge is `<span class="phux-agent-badge" data-tone=… data-status=…>`
//! holding an inline SVG mark (shape carries the state, so it reads in
//! monochrome and in forced colours), the provider and the vocabulary word.
//! The list is not a live region; entering `needs you` or `failed` is
//! announced once, politely, on the pane's own announcement node.

use std::collections::{HashMap, HashSet};

use phux_design::{Agent, Link, State, ToneInput, tone_of, word};
use phux_protocol::ids::ResourceId;

use super::*;
use crate::session::PaneExit;

/// DOM id of the polite live region that announces a pane needing you.
pub const ANNOUNCE_ID: &str = "phux-agent-announce";

/// Class of the bell's unread dot, shown under reduce motion instead of a
/// wash until the next input.
pub const BELL_DOT_CLASS: &str = "phux-bell-dot";

/// Visually hidden, still read: the announcement node.
const VISUALLY_HIDDEN: &str = "position:absolute;inline-size:1px;block-size:1px;\
    overflow:hidden;clip-path:inset(50%);white-space:nowrap;";

/// What the badges remember between paints: each badge's last ladder state,
/// for announcing transitions, and the done badges the user has since typed
/// at (done, seen).
#[derive(Default)]
pub(super) struct BadgeMemory {
    states: HashMap<ResourceId, State>,
    seen: HashSet<ResourceId>,
}

impl BadgeMemory {
    /// Place badge `id` on the ladder, remembering it. Returns its state
    /// and the state it moved from, when it moved (a badge seen for the
    /// first time has not moved).
    pub(super) fn settle(
        &mut self,
        id: &ResourceId,
        word: &str,
        exit: Option<PaneExit>,
        link_lost: bool,
    ) -> (State, Option<State>) {
        let state = ladder(word, exit, link_lost, self.seen.contains(id));
        // "Seen" belongs to one finish: leaving done forgets it.
        if !matches!(state, State::Done | State::DoneSeen) {
            self.seen.remove(id);
        }
        let before = self.states.insert(id.clone(), state);
        (state, before.filter(|before| *before != state))
    }

    /// Forget badges no longer on screen.
    pub(super) fn keep_only(&mut self, ids: &[&ResourceId]) {
        self.states.retain(|id, _| ids.contains(&id));
        self.seen.retain(|id| ids.contains(&id));
    }

    /// The user typed: every finished agent on screen is now seen. Returns
    /// whether any changed.
    pub(super) fn see_done(&mut self) -> bool {
        let done: Vec<ResourceId> = self
            .states
            .iter()
            .filter(|(_, state)| **state == State::Done)
            .map(|(id, _)| id.clone())
            .collect();
        let changed = !done.is_empty();
        self.seen.extend(done);
        changed
    }
}

/// The kernel's lifecycle word as an agent state. `ended` (the provider
/// session ended) has no agent state; it is drawn as `ended`.
#[must_use]
pub fn agent_of(word: &str) -> Option<Agent> {
    Some(match word {
        "working" => Agent::Working,
        "blocked" => Agent::Blocked,
        "idle" => Agent::Idle,
        "done" => Agent::Done,
        "unknown" | "" => Agent::Unknown,
        _ => return None,
    })
}

/// The ladder state of one badge: its agent word, its pane's exit, the
/// transport, and whether the user has seen it finish.
#[must_use]
pub fn ladder(state: &str, exit: Option<PaneExit>, link_lost: bool, seen: bool) -> State {
    let input = ToneInput {
        agent: agent_of(state),
        exit: exit.and_then(|exit| exit.code),
        crashed: exit.is_some_and(|exit| exit.signal.is_some()),
        link: link_lost.then_some(Link::Lost),
        seen,
        ..ToneInput::default()
    };
    match tone_of(&input) {
        State::Unknown if state == "ended" => State::Ended,
        found => found,
    }
}

/// The vector mark for a ladder state, on a 16-point grid in
/// `currentColor` (the Rune marks, `clients/rune/src/ui/icons.rs`).
#[must_use]
pub const fn mark(state: State) -> &'static str {
    match state {
        State::NeedsYou => r#"<circle cx="8" cy="8" r="4.5" fill="currentColor"/>"#,
        State::Failed | State::Lost => {
            r#"<path d="M4.75 4.75l6.5 6.5M11.25 4.75l-6.5 6.5" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>"#
        }
        State::Done => r#"<path d="M8 2.75L13.25 8 8 13.25 2.75 8z" fill="currentColor"/>"#,
        State::Working => {
            r#"<circle cx="8" cy="8" r="4.3" fill="none" stroke="currentColor" stroke-width="1.4"/><path d="M8 3.7a4.3 4.3 0 0 0 0 8.6z" fill="currentColor"/>"#
        }
        State::DoneSeen | State::Idle => {
            r#"<circle cx="8" cy="8" r="4.3" fill="none" stroke="currentColor" stroke-width="1.4"/>"#
        }
        // Eight dots on the ring, as Rune draws it: a dashed stroke at
        // badge size rasterises close to solid and reads as idle.
        State::Unknown => {
            r#"<circle cx="8" cy="3.7" r="0.8" fill="currentColor"/><circle cx="11.04" cy="4.96" r="0.8" fill="currentColor"/><circle cx="12.3" cy="8" r="0.8" fill="currentColor"/><circle cx="11.04" cy="11.04" r="0.8" fill="currentColor"/><circle cx="8" cy="12.3" r="0.8" fill="currentColor"/><circle cx="4.96" cy="11.04" r="0.8" fill="currentColor"/><circle cx="3.7" cy="8" r="0.8" fill="currentColor"/><circle cx="4.96" cy="4.96" r="0.8" fill="currentColor"/>"#
        }
        State::Ended => {
            r#"<path d="M4.5 8h7" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>"#
        }
    }
}

/// The bell's mark.
const BELL_MARK: &str = r#"<path d="M4 11.5V7a4 4 0 0 1 8 0v4.5l1 1H3zM6.5 14h3" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/>"#;

fn svg(body: &str) -> String {
    format!(
        "<svg class='phux-mark' viewBox='0 0 16 16' aria-hidden='true' focusable='false'>{body}</svg>"
    )
}

/// The text a screen reader hears when a pane enters `state`, if any:
/// only `needs you`, `failed` and `lost` are announced (§1.3 rule 9).
#[must_use]
pub fn announcement(provider: &str, state: State) -> Option<String> {
    matches!(state, State::NeedsYou | State::Failed | State::Lost)
        .then(|| format!("{provider} {}", state.word().unwrap_or_default()))
}

impl App {
    /// Project the focused pane's agent badges into the DOM: one
    /// `<span class="phux-agent-badge">` per `AgentSession` under a
    /// `#phux-agent-badges` container beside the canvas, hidden when empty,
    /// plus the bell's unread dot.
    pub(super) fn paint_badges(&self) {
        let Some(container) = self.badge_container() else {
            return;
        };
        let Some(document) = container.owner_document() else {
            return;
        };
        let badges = self.session.agent_badges();
        let focused = self.session.focused_pane();
        let link_lost = self.failure_reason.borrow().is_some();
        let mut memory = self.badges.borrow_mut();
        container.set_text_content(None);
        let mut said = Vec::new();
        for badge in &badges {
            let exit = badge
                .parent
                .as_ref()
                .or(focused.as_ref())
                .and_then(|pane| self.session.pane_exit(pane));
            let (state, moved) = memory.settle(&badge.id, &badge.state, exit, link_lost);
            let provider = if badge.provider.is_empty() {
                "agent"
            } else {
                badge.provider.as_str()
            };
            // Once per transition, never for a badge seen for the first time.
            if moved.is_some()
                && let Some(line) = announcement(provider, state)
            {
                said.push(line);
            }
            if let Some(span) = badge_element(&document, badge, provider, state) {
                let _ = container.append_child(&span);
            }
        }
        memory.keep_only(&badges.iter().map(|badge| &badge.id).collect::<Vec<_>>());
        drop(memory);
        if self.bell_unread.get()
            && let Some(dot) = bell_dot(&document)
        {
            let _ = container.append_child(&dot);
        }
        let _ = container.set_attribute("hidden", "");
        if container.child_element_count() > 0 {
            let _ = container.remove_attribute("hidden");
        }
        if !said.is_empty() {
            self.announce(&said.join(". "));
        }
    }

    /// The user typed at the terminal: the bell is read, and every agent on
    /// screen that finished is now seen.
    pub(super) fn acknowledge_input(&self) {
        let bell = self.bell_unread.replace(false);
        let seen = self.badges.borrow_mut().see_done();
        if bell || seen {
            self.paint_badges();
        }
    }

    /// Say `text` once on the polite announcement node beside the canvas.
    fn announce(&self, text: &str) {
        let Some(document) = self.canvas.owner_document() else {
            return;
        };
        let node = document.get_element_by_id(ANNOUNCE_ID).or_else(|| {
            let node = document.create_element("div").ok()?;
            node.set_id(ANNOUNCE_ID);
            node.set_attribute("aria-live", "polite").ok()?;
            node.set_attribute("style", VISUALLY_HIDDEN).ok()?;
            let parent = self
                .canvas
                .parent_element()
                .or_else(|| document.body().map(Into::into))?;
            parent.append_child(&node).ok()?;
            Some(node)
        });
        if let Some(node) = node {
            node.set_text_content(Some(text));
        }
    }

    /// The badge container, created next to the canvas on first use.
    fn badge_container(&self) -> Option<web_sys::Element> {
        let document = self.canvas.owner_document()?;
        if let Some(existing) = document.get_element_by_id(BADGE_CONTAINER_ID) {
            return Some(existing);
        }
        let container = document.create_element("div").ok()?;
        container.set_id(BADGE_CONTAINER_ID);
        let _ = container.set_attribute("hidden", "");
        let parent = self
            .canvas
            .parent_element()
            .or_else(|| document.body().map(Into::into))?;
        parent.append_child(&container).ok()?;
        Some(container)
    }
}

/// One badge: mark, provider and word. `data-state` keeps the kernel's
/// word; `data-status` is the ladder state and `data-tone` its colour.
pub(super) fn badge_element(
    document: &web_sys::Document,
    badge: &crate::AgentBadge,
    provider: &str,
    state: State,
) -> Option<web_sys::Element> {
    let span = document.create_element("span").ok()?;
    span.set_class_name("phux-agent-badge");
    span.set_attribute("data-provider", &badge.provider).ok()?;
    span.set_attribute("data-state", &badge.state).ok()?;
    span.set_attribute("data-status", state.key()).ok()?;
    span.set_attribute("data-tone", state.tone().key()).ok()?;
    // Static markup only; the provider and word go in as text below.
    span.set_inner_html(&svg(mark(state)));
    let name = document.create_element("span").ok()?;
    name.set_class_name("phux-agent-provider");
    name.set_text_content(Some(provider));
    span.append_child(&name).ok()?;
    if let Some(word) = state.word() {
        // A space keeps the accessible name "claude needs you"; flex
        // layout drops it, so the gap still comes from the stylesheet.
        span.insert_adjacent_text("beforeend", " ").ok()?;
        let label = document.create_element("span").ok()?;
        label.set_class_name("phux-agent-word");
        label.set_text_content(Some(word));
        span.append_child(&label).ok()?;
    }
    Some(span)
}

/// The bell's unread dot: its mark, named by the vocabulary's word.
pub(super) fn bell_dot(document: &web_sys::Document) -> Option<web_sys::Element> {
    let dot = document.create_element("span").ok()?;
    dot.set_class_name(BELL_DOT_CLASS);
    dot.set_attribute("data-tone", "bell").ok()?;
    dot.set_attribute("role", "img").ok()?;
    dot.set_attribute("aria-label", word("bell").unwrap_or_default())
        .ok()?;
    dot.set_inner_html(&svg(BELL_MARK));
    // Mark and word, never the mark alone (§1.3 rule 1).
    let label = document.create_element("span").ok()?;
    label.set_class_name("phux-agent-word");
    label.set_text_content(Some(word("bell").unwrap_or_default()));
    dot.append_child(&label).ok()?;
    Some(dot)
}
