//! The diff column: a toolbar over Kit's diff view of every file in the shown
//! comparison. Words that changed inside a line are emphasized, syntax is
//! colored, unchanged runs fold with expand controls, and wide panels show the
//! two sides apart. Each file header carries its viewed mark and stage
//! control; selected lines can be staged alone or commented on for the agent.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, ElementId, Entity, IntoElement, SharedString, WeakEntity, div, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::diff::{
    Diff, DiffAnnotation, DiffChangeIndicator, DiffFile, DiffFileStatus, DiffHoverHighlight,
    DiffLinePosition, DiffLineRange, DiffState,
};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::{Selectable, Sizable};

use crate::git::Source;

use super::super::super::changes::review::range_label;
use super::super::super::color::from_hex;
use super::super::super::desk::Desk;
use super::super::super::skin::Look;
use super::super::super::ui::{ghost_button, ghost_icon, muted};

pub(super) fn column(desk: &Desk, width: f32, look: &Look, cx: &mut Context<Desk>) -> AnyElement {
    let shown = desk.changes.diff.as_ref().map(|(state, _)| state.clone());
    let empty = desk.changes.loaded(desk.changes.source).files.is_empty();
    let body = match shown {
        Some(state) if !empty => {
            let mode = desk.changes.layout.mode(width);
            if state.read(cx).mode() != mode {
                state.update(cx, |state, cx| state.set_mode(mode, cx));
            }
            diff(desk, &state, look, cx).into_any_element()
        }
        _ => div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .child(muted(look, "No changes in this comparison"))
            .into_any_element(),
    };
    div()
        .debug_selector(|| "changes-diff-column".into())
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .child(toolbar(desk, look, cx))
        .child(body)
        .into_any_element()
}

/// Which comparison, review progress, change navigation, line staging and
/// the review's send button.
fn toolbar(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let source = desk.changes.source;
    let count = |source| desk.changes.loaded(source).files.len();
    let (viewed, total) = desk.changes.progress();
    let selection = desk
        .changes
        .diff
        .as_ref()
        .and_then(|(state, _)| state.read(cx).selected_lines());
    let (stage_label, stage_tip) = match source {
        Source::Changes => ("Stage Lines", "Stage the selected lines"),
        Source::Staged => ("Unstage Lines", "Unstage the selected lines"),
    };
    let comments = desk.changes.comments.len();
    let loaded = desk.changes.loaded(source);
    let note = if !loaded.unreadable.is_empty() {
        Some(format!("Can't show {}", loaded.unreadable.join(", ")))
    } else if loaded.reduced {
        Some("Large diff: context is limited".to_owned())
    } else {
        None
    };
    let tab = |id: &'static str, label: &str, target: Source| {
        toolbar_action(
            id,
            Button::new(id)
                .label(format!("{label} {}", count(target)))
                .ghost()
                .xsmall()
                .selected(source == target)
                .on_click(cx.listener(move |desk, _, _, cx| desk.set_change_source(target, cx))),
        )
    };
    div()
        .debug_selector(|| "changes-diff-toolbar".into())
        .w_full()
        .min_w_0()
        .flex()
        .flex_wrap()
        .flex_shrink_0()
        .items_center()
        .gap(px(4.))
        .px(px(8.))
        .py(px(6.))
        .border_b_1()
        .border_color(look.divider)
        .child(tab("changes-source-changes", "Changes", Source::Changes))
        .child(tab("changes-source-staged", "Staged", Source::Staged))
        .when(total > 0, |bar| {
            bar.child(muted(look, format!("{viewed}/{total} viewed")).ml(px(6.)))
        })
        .children(note.map(|note| muted(look, note).w_full().min_w_0().truncate()))
        .when(selection.is_some(), |bar| {
            bar.child(toolbar_action(
                "changes-stage-lines",
                ghost_button("changes-stage-lines", stage_label)
                    .xsmall()
                    .tooltip(stage_tip)
                    .disabled(desk.changes.busy)
                    .on_click(cx.listener(|desk, _, _, cx| desk.stage_selected_lines(cx))),
            ))
        })
        .child(toolbar_action(
            "changes-previous",
            ghost_icon("changes-previous", IconName::ArrowUp, "Previous Change").on_click(
                cx.listener(|desk, _, _, cx| {
                    if let Some((state, _)) = &desk.changes.diff {
                        state.update(cx, |state, cx| state.previous_change(cx));
                    }
                }),
            ),
        ))
        .child(toolbar_action(
            "changes-next",
            ghost_icon("changes-next", IconName::ArrowDown, "Next Change").on_click(cx.listener(
                |desk, _, _, cx| {
                    if let Some((state, _)) = &desk.changes.diff {
                        state.update(cx, |state, cx| state.next_change(cx));
                    }
                },
            )),
        ))
        .when(comments > 0, |bar| {
            let label = if comments == 1 {
                "Send 1 Note".to_owned()
            } else {
                format!("Send {comments} Notes")
            };
            bar.child(toolbar_action(
                "changes-send-review",
                Button::new("changes-send-review")
                    .icon(IconName::Send)
                    .label(label)
                    .primary()
                    .xsmall()
                    .tooltip("Paste the review into the agent's terminal")
                    .on_click(cx.listener(|desk, _, _, cx| desk.send_review(cx))),
            ))
        })
}

/// Keep whole actions together when the toolbar wraps into a narrow diff column.
fn toolbar_action(selector: &'static str, button: Button) -> impl IntoElement {
    div()
        .debug_selector(move || selector.into())
        .flex_shrink_0()
        .child(button)
}

/// Colors an annotation paints with; the renderer runs without the desk.
#[derive(Clone, Copy)]
struct Ink {
    card: gpui::Hsla,
    border: gpui::Hsla,
    muted: gpui::Hsla,
    radius: f32,
}

/// What a note slot under some lines holds.
#[derive(Clone)]
enum Note {
    Draft(Entity<TextareaState>, String),
    Saved(u64, String),
}

/// A note slot under the last line of `range`.
fn slot(id: String, range: &DiffLineRange) -> DiffAnnotation {
    let line = DiffLinePosition::new(range.path().clone(), range.end_side(), range.end());
    DiffAnnotation::line(ElementId::from(SharedString::from(id)), line)
}

/// The shown comparison's saved notes, then the one being written.
fn notes(desk: &Desk) -> Vec<(DiffAnnotation, Note)> {
    let saved = desk.changes.shown_comments().map(|comment| {
        let note = Note::Saved(comment.id, comment.body.clone());
        (slot(format!("note-{}", comment.id), &comment.range), note)
    });
    let draft = desk
        .changes
        .draft
        .as_ref()
        .filter(|draft| draft.source == desk.changes.source);
    let draft = draft.map(|draft| {
        let note = Note::Draft(draft.input.clone(), range_label(&draft.range));
        (slot("note-draft".into(), &draft.range), note)
    });
    saved.chain(draft).collect()
}

fn diff(
    desk: &Desk,
    state: &Entity<DiffState>,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let handle = cx.weak_entity();
    let (source, busy) = (desk.changes.source, desk.changes.busy);
    let viewed: Rc<HashSet<String>> = Rc::new(
        desk.changes
            .loaded(source)
            .sections
            .keys()
            .filter(|path| desk.changes.is_viewed(path))
            .cloned()
            .collect(),
    );
    let ink = Ink {
        card: look.hover,
        border: look.border,
        muted: from_hex(&look.chrome.muted),
        radius: look.skin.shape.kit_radius,
    };
    let notes = Rc::new(notes(desk));
    let (header_handle, header_state) = (handle.clone(), state.clone());
    let (note_handle, add_handle) = (handle.clone(), handle);
    let slots = notes
        .iter()
        .map(|(slot, _)| slot.clone())
        .collect::<Vec<_>>();
    Diff::new(state)
        .flex_1()
        .min_h_0()
        .hover_highlight(DiffHoverHighlight::Both)
        .change_indicator(DiffChangeIndicator::Bars)
        .render_header_suffix(move |file, _, cx| {
            let viewed = viewed.contains(file.path().as_ref());
            header_controls(
                file,
                source,
                viewed,
                busy,
                &header_state,
                &header_handle,
                cx,
            )
        })
        .annotations(slots)
        .render_annotation(move |slot, _, _| {
            let note = notes.iter().find(|(each, _)| each.id() == slot.id());
            match note.map(|(_, note)| note.clone()) {
                Some(Note::Draft(input, label)) => {
                    draft_card(input, label, ink, note_handle.clone())
                }
                Some(Note::Saved(id, body)) => note_card(id, body, ink, note_handle.clone()),
                None => div().into_any_element(),
            }
        })
        .on_add_annotation(move |range, window, cx| {
            let range = range.clone();
            let _ = add_handle.update(cx, |desk, cx| desk.start_comment(range, window, cx));
        })
}

/// A file header's viewed mark and stage, unstage or resolve button.
fn header_controls(
    file: &DiffFile,
    source: Source,
    viewed: bool,
    busy: bool,
    state: &Entity<DiffState>,
    handle: &WeakEntity<Desk>,
    cx: &App,
) -> AnyElement {
    let path = file.path().to_string();
    let conflicted = file.status() == DiffFileStatus::Conflicted;
    let (label, ready) = match (conflicted, source) {
        (true, _) => (
            "Mark Resolved",
            state.read(cx).resolved_text(&path).is_some(),
        ),
        (false, Source::Changes) => ("Stage", true),
        (false, Source::Staged) => ("Unstage", true),
    };
    let viewed_path = path.clone();
    let viewed_handle = handle.clone();
    let stage_handle = handle.clone();
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .child(
            Button::new(SharedString::from(format!("changes-viewed-{path}")))
                .label("Viewed")
                .when(viewed, |button| button.icon(IconName::Check))
                .ghost()
                .xsmall()
                .selected(viewed)
                .on_click(move |_, _, cx| {
                    let _ =
                        viewed_handle.update(cx, |desk, cx| desk.toggle_viewed(&viewed_path, cx));
                }),
        )
        .child(
            Button::new(SharedString::from(format!("changes-file-stage-{path}")))
                .label(label)
                .ghost()
                .xsmall()
                .disabled(busy || !ready)
                .on_click(move |_, _, cx| {
                    let _ = stage_handle.update(cx, |desk, cx| desk.stage_path(source, &path, cx));
                }),
        )
        .into_any_element()
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

fn note_card(id: u64, body: String, ink: Ink, handle: WeakEntity<Desk>) -> AnyElement {
    card(ink)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(ink.muted)
                        .child("Note for the agent"),
                )
                .child(
                    ghost_icon(
                        SharedString::from(format!("changes-note-remove-{id}")),
                        IconName::Close,
                        "Remove Note",
                    )
                    .on_click(move |_, _, cx| {
                        let _ = handle.update(cx, |desk, cx| desk.remove_comment(id, cx));
                    }),
                ),
        )
        .child(div().whitespace_normal().child(body))
        .into_any_element()
}

fn draft_card(
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
                .aria_label("Note for the agent")
                .w_full(),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .gap(px(6.))
                .child(
                    ghost_button("changes-note-cancel", "Cancel")
                        .xsmall()
                        .on_click(move |_, _, cx| {
                            let _ = cancel.update(cx, |desk, cx| desk.cancel_comment(cx));
                        }),
                )
                .child(
                    Button::new("changes-note-save")
                        .label("Add Note")
                        .primary()
                        .xsmall()
                        .on_click(move |_, _, cx| {
                            let _ = handle.update(cx, |desk, cx| desk.save_comment(cx));
                        }),
                ),
        )
        .into_any_element()
}
