//! What sits under a diff's lines: GitHub's threads with their replies and
//! resolution, the comments kept for the review, and the one being written.

use std::collections::HashSet;

use gpui::prelude::*;
use gpui::{
    AnyElement, ElementId, Entity, FontWeight, IntoElement, SharedString, WeakEntity, div, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::Disableable;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::diff::{DiffAnnotation, DiffLinePosition, DiffSide};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::text::TextView;

use crate::git::github::{LineSide, Note, Thread};

use super::super::super::changes::review::range_label;
use super::super::super::desk::Desk;
use super::super::super::reviews::{DraftFor, Opened};
use super::super::super::ui::{ghost_button, ghost_icon};
use super::ago;

/// Colors an annotation paints with; the renderer runs without the desk.
#[derive(Clone, Copy)]
pub(super) struct Ink {
    pub(super) card: gpui::Hsla,
    pub(super) border: gpui::Hsla,
    pub(super) muted: gpui::Hsla,
    pub(super) accent: gpui::Hsla,
    pub(super) warning: gpui::Hsla,
    pub(super) radius: f32,
}

/// What one slot under some lines holds.
#[derive(Clone)]
pub(super) enum Slot {
    Thread {
        thread: Thread,
        expanded: bool,
        reply: Option<Entity<TextareaState>>,
        busy: bool,
    },
    Pending(u64, String, String),
    Draft(Entity<TextareaState>, String),
}

const fn kit_side(side: LineSide) -> DiffSide {
    match side {
        LineSide::Left => DiffSide::Original,
        LineSide::Right => DiffSide::Modified,
    }
}

pub(super) fn slots(opened: &Opened) -> Vec<(DiffAnnotation, Slot)> {
    let mut slots = Vec::new();
    let reply_to = match opened
        .draft
        .as_ref()
        .map(|draft| (&draft.target, &draft.input))
    {
        Some((DraftFor::Reply(thread), input)) => Some((thread.clone(), input.clone())),
        _ => None,
    };
    let shown: HashSet<&str> = opened
        .patch
        .data
        .iter()
        .flat_map(|patch| patch.loaded.files.iter().map(|file| file.path().as_ref()))
        .collect();
    for thread in opened.pull.data.iter().flat_map(|pull| pull.threads.iter()) {
        if !shown.contains(thread.path.as_str()) {
            continue;
        }
        let id = ElementId::from(SharedString::from(format!("thread-{}", thread.id)));
        // An outdated thread's lines are gone; it sits under the file header.
        let annotation = match thread.line.filter(|_| !thread.outdated) {
            Some(line) => DiffAnnotation::line(
                id,
                DiffLinePosition::new(thread.path.clone(), kit_side(thread.side), line as usize),
            ),
            None => DiffAnnotation::file(id, thread.path.clone()),
        };
        let reply = reply_to
            .as_ref()
            .filter(|(target, _)| *target == thread.id)
            .map(|(_, input)| input.clone());
        slots.push((
            annotation,
            Slot::Thread {
                thread: thread.clone(),
                expanded: !thread.resolved || opened.expanded.contains(&thread.id),
                reply,
                busy: opened.busy.is_some(),
            },
        ));
    }
    for pending in &opened.pending {
        let range = &pending.range;
        let line = DiffLinePosition::new(range.path().clone(), range.end_side(), range.end());
        let id = ElementId::from(SharedString::from(format!("pending-{}", pending.id)));
        slots.push((
            DiffAnnotation::line(id, line),
            Slot::Pending(pending.id, range_label(range), pending.body.clone()),
        ));
    }
    if let Some(draft) = opened.draft.as_ref()
        && let DraftFor::Lines(range) = &draft.target
    {
        let line = DiffLinePosition::new(range.path().clone(), range.end_side(), range.end());
        slots.push((
            DiffAnnotation::line(ElementId::from(SharedString::from("review-draft")), line),
            Slot::Draft(draft.input.clone(), range_label(range)),
        ));
    }
    slots
}

fn card(ink: Ink) -> gpui::Div {
    div()
        .m(px(6.))
        .p(px(8.))
        .flex()
        .flex_col()
        .gap(px(6.))
        .rounded(px(ink.radius))
        .bg(ink.card)
        .border_1()
        .border_color(ink.border)
        .text_sm()
}

fn note(note: &Note, ink: Ink) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .flex()
                .gap(px(6.))
                .text_xs()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(note.author.clone()),
                )
                .child(div().text_color(ink.muted).child(ago(&note.created_at))),
        )
        .child(
            TextView::markdown(
                SharedString::from(format!("review-note-{}", note.id)),
                note.body.clone(),
            )
            .selectable(true)
            .scrollable(false),
        )
}

pub(super) fn thread_card(
    thread: &Thread,
    expanded: bool,
    reply: Option<Entity<TextareaState>>,
    busy: bool,
    ink: Ink,
    handle: WeakEntity<Desk>,
) -> AnyElement {
    let id = thread.id.clone();
    let first = thread.comments.first();
    let mut state = Vec::new();
    if thread.resolved {
        state.push("Resolved");
    }
    if thread.outdated {
        state.push("Outdated");
    }
    let heading = div()
        .flex()
        .items_center()
        .gap(px(6.))
        .text_xs()
        .text_color(ink.muted)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(match (expanded, first) {
                    (false, Some(first)) => format!(
                        "{} · {}: {}",
                        state.join(" · "),
                        first.author,
                        first.body.lines().next().unwrap_or_default()
                    ),
                    _ if state.is_empty() => format!("{} comments", thread.comments.len()),
                    _ => state.join(" · "),
                }),
        )
        .when(thread.resolved, |row| {
            let toggle = handle.clone();
            let id = id.clone();
            row.child(
                ghost_button(
                    SharedString::from(format!("thread-show-{id}")),
                    if expanded { "Hide" } else { "Show" },
                )
                .xsmall()
                .on_click(move |_, _, cx| {
                    let _ = toggle.update(cx, |desk, cx| desk.toggle_thread_expanded(&id, cx));
                }),
            )
        });
    let mut card = card(ink)
        .when(!thread.resolved, |card| {
            card.border_color(ink.warning.opacity(0.5))
        })
        .child(heading);
    if !expanded {
        return card.into_any_element();
    }
    card = card.children(thread.comments.iter().map(|each| note(each, ink)));
    if let Some(input) = reply {
        let (cancel, save) = (handle.clone(), handle.clone());
        return card
            .child(Textarea::new(&input).aria_label("Reply").w_full())
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(6.))
                    .child(
                        ghost_button("thread-reply-cancel", "Cancel")
                            .xsmall()
                            .on_click(move |_, _, cx| {
                                let _ = cancel.update(cx, |desk, cx| desk.cancel_review_draft(cx));
                            }),
                    )
                    .child(
                        Button::new("thread-reply-send")
                            .label("Reply")
                            .primary()
                            .xsmall()
                            .disabled(busy)
                            .on_click(move |_, _, cx| {
                                let _ = save.update(cx, |desk, cx| desk.save_review_draft(cx));
                            }),
                    ),
            )
            .into_any_element();
    }
    let rights = thread.rights;
    let (reply_handle, resolve_handle) = (handle.clone(), handle);
    let (reply_id, resolve_id) = (id.clone(), id.clone());
    let resolved = thread.resolved;
    card.child(
        div()
            .flex()
            .justify_end()
            .gap(px(6.))
            .when(rights.reply, |row| {
                row.child(
                    ghost_button(SharedString::from(format!("thread-reply-{id}")), "Reply")
                        .xsmall()
                        .on_click(move |_, window, cx| {
                            let thread = reply_id.clone();
                            let _ = reply_handle
                                .update(cx, |desk, cx| desk.start_reply(thread, window, cx));
                        }),
                )
            })
            .when(
                (rights.resolve && !resolved) || (rights.unresolve && resolved),
                |row| {
                    row.child(
                        ghost_button(
                            SharedString::from(format!("thread-resolve-{id}")),
                            if resolved { "Reopen" } else { "Resolve" },
                        )
                        .xsmall()
                        .disabled(busy)
                        .on_click(move |_, _, cx| {
                            let thread = resolve_id.clone();
                            let _ = resolve_handle.update(cx, |desk, cx| {
                                desk.set_thread_resolved(thread, !resolved, cx)
                            });
                        }),
                    )
                },
            ),
    )
    .into_any_element()
}

pub(super) fn pending_card(
    id: u64,
    label: String,
    body: String,
    ink: Ink,
    handle: WeakEntity<Desk>,
) -> AnyElement {
    card(ink)
        .border_color(ink.accent.opacity(0.6))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(ink.accent)
                        .child(format!("Pending · {label}")),
                )
                .child(
                    ghost_icon(
                        SharedString::from(format!("pending-remove-{id}")),
                        IconName::Close,
                        "Remove Comment",
                    )
                    .on_click(move |_, _, cx| {
                        let _ = handle.update(cx, |desk, cx| desk.remove_pending(id, cx));
                    }),
                ),
        )
        .child(div().whitespace_normal().child(body))
        .into_any_element()
}

pub(super) fn draft_card(
    input: Entity<TextareaState>,
    label: String,
    ink: Ink,
    handle: WeakEntity<Desk>,
) -> AnyElement {
    let cancel = handle.clone();
    card(ink)
        .child(div().text_xs().text_color(ink.muted).child(label))
        .child(
            Textarea::new(&input)
                .aria_label("Comment for the review")
                .w_full(),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .gap(px(6.))
                .child(
                    ghost_button("review-draft-cancel", "Cancel")
                        .xsmall()
                        .on_click(move |_, _, cx| {
                            let _ = cancel.update(cx, |desk, cx| desk.cancel_review_draft(cx));
                        }),
                )
                .child(
                    Button::new("review-draft-save")
                        .label("Add to Review")
                        .primary()
                        .xsmall()
                        .on_click(move |_, _, cx| {
                            let _ = handle.update(cx, |desk, cx| desk.save_review_draft(cx));
                        }),
                ),
        )
        .into_any_element()
}
