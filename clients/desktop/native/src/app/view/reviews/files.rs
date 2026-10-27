//! The Files tab: every changed file with its viewed mark, and Kit's diff of
//! all of them with [`super::threads`] under their lines.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, IntoElement, SharedString, Window, div, px};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::diff::{Diff, DiffAnnotation, DiffChangeIndicator, DiffHoverHighlight};
use gpui_kit::component::{Selectable, Sizable};

use super::super::super::color::from_hex;
use super::super::super::desk::Desk;
use super::super::super::reviews::{Opened, PatchFrom};
use super::super::super::skin::Look;
use super::super::super::ui::{ghost_icon, muted};
use super::placeholder;
use super::threads::{Ink, Slot, draft_card, pending_card, slots, thread_card};

/// The file list's width, and the share of a narrow window it takes at most.
const LIST_WIDTH: f32 = 260.;
const LIST_SHARE: f32 = 0.18;

fn list_width(viewport: f32) -> f32 {
    LIST_WIDTH.min(viewport * LIST_SHARE)
}

pub(super) fn files(
    desk: &Desk,
    opened: &Opened,
    window: &mut Window,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let viewport = window.viewport_size().width.as_f32();
    let list_width = list_width(viewport);
    let diff = match (&opened.patch.data, &opened.patch.error, &opened.diff) {
        (Some(patch), _, Some((state, _))) if !patch.loaded.files.is_empty() => {
            let gutter = look.skin.shape.gutter;
            let width = viewport - super::inbox_width(viewport) - list_width - 2. * gutter;
            let mode = super::super::super::reviews::diff_mode(desk.reviews.layout, width);
            if state.read(cx).mode() != mode {
                state.update(cx, |state, cx| state.set_mode(mode, cx));
            }
            column(desk, opened, state, look, cx)
        }
        (Some(_), _, _) => placeholder("No changes", "This pull request changes no file.", look),
        (None, Some(error), _) => placeholder("Could not read the diff", error, look),
        (None, None, _) => placeholder("Reading the diff…", "", look),
    };
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .child(list(opened, list_width, look, cx))
        .child(diff)
        .into_any_element()
}

/// Every file the pull request changes, with its mark and open threads.
fn list(opened: &Opened, width: f32, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let Some(pull) = opened.pull.data.as_ref() else {
        return div().into_any_element();
    };
    div()
        .id("reviews-files")
        .w(px(width))
        .flex_shrink_0()
        .h_full()
        .overflow_y_scroll()
        .border_r_1()
        .border_color(look.divider)
        .py(px(6.))
        .px(px(6.))
        .children(pull.files.iter().map(|file| {
            let viewed = opened.is_viewed(&file.path);
            let dismissed = opened.is_dismissed(&file.path);
            let threads = pull
                .threads
                .iter()
                .filter(|thread| thread.path == file.path && !thread.resolved)
                .count();
            let focused = opened.focus.as_deref() == Some(file.path.as_str());
            let (name, folder) = match file.path.rsplit_once('/') {
                Some((folder, name)) => (name.to_owned(), folder.to_owned()),
                None => (file.path.clone(), String::new()),
            };
            let path = file.path.clone();
            let mark = file.path.clone();
            let id = SharedString::from(format!("reviews-file-{}", file.path));
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(6.))
                .py(px(3.))
                .rounded(px(look.skin.shape.kit_radius))
                .cursor_pointer()
                .hover(|row| row.bg(look.hover))
                .when(focused, |row| row.bg(look.active))
                .on_click(cx.listener(move |desk, _, _, cx| desk.reveal_review_file(&path, cx)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .text_size(px(12.5))
                                .text_color(if viewed {
                                    from_hex(&look.chrome.muted)
                                } else {
                                    look.ink
                                })
                                .child(name),
                        )
                        .when(!folder.is_empty(), |cell| {
                            cell.child(muted(look, folder).truncate())
                        }),
                )
                .when(threads > 0, |row| {
                    let warning = from_hex(&look.chrome.theme.warning);
                    row.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(2.))
                            .text_xs()
                            .text_color(warning)
                            .child(
                                gpui_kit::component::Icon::new(IconName::MessageSquare)
                                    .xsmall()
                                    .text_color(warning),
                            )
                            .child(format!("{threads}")),
                    )
                })
                .when(dismissed, |row| {
                    row.child(
                        muted(look, "changed").text_color(from_hex(&look.chrome.theme.warning)),
                    )
                })
                .child(super::line_counts(file.additions, file.deletions, look))
                .child(
                    ghost_icon(
                        SharedString::from(format!("reviews-file-viewed-{}", file.path)),
                        if viewed {
                            IconName::CircleCheck
                        } else {
                            IconName::Eye
                        },
                        if viewed {
                            "Mark Unviewed"
                        } else {
                            "Mark Viewed"
                        },
                    )
                    .selected(viewed)
                    .on_click(cx.listener(move |desk, _, _, cx| {
                        cx.stop_propagation();
                        desk.toggle_review_viewed(&mark, cx);
                    })),
                )
        }))
        .into_any_element()
}

/// The diff column: where the patch came from, progress, change steps, then the diff.
fn column(
    desk: &Desk,
    opened: &Opened,
    state: &Entity<gpui_kit::component::diff::DiffState>,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let (viewed, total) = opened.progress();
    let source = match opened.patch.data.as_ref().map(|patch| &patch.from) {
        Some(PatchFrom::Local(root)) => format!("Full context from {}", root.display()),
        _ => "GitHub's diff: three lines of context. Clone the repository for more.".to_owned(),
    };
    let reduced = opened
        .patch
        .data
        .as_ref()
        .is_some_and(|patch| patch.loaded.reduced);
    let unreadable = opened
        .patch
        .data
        .as_ref()
        .map(|patch| patch.loaded.unreadable.join(", "))
        .filter(|paths| !paths.is_empty());
    let source = match unreadable {
        Some(paths) => format!("Can't show {paths}"),
        None => source,
    };
    let toolbar = div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(10.))
        .py(px(5.))
        .border_b_1()
        .border_color(look.divider)
        .child(muted(look, format!("{viewed}/{total} viewed")))
        .child(
            muted(
                look,
                if reduced {
                    "Large diff: context is limited".to_owned()
                } else {
                    source
                },
            )
            .flex_1()
            .min_w_0()
            .truncate(),
        )
        .children((!opened.pending.is_empty()).then(|| {
            muted(
                look,
                format!("{} kept for the review", opened.pending.len()),
            )
            .text_color(from_hex(&look.chrome.theme.accent))
        }))
        .child({
            let (icon, label) = super::super::changes::layout_control(desk.reviews.layout);
            ghost_icon("reviews-layout", icon, label)
                .selected(desk.reviews.layout != super::super::super::changes::Layout::Auto)
                .on_click(cx.listener(|desk, _, _, cx| {
                    desk.reviews.layout = desk.reviews.layout.next();
                    cx.notify();
                }))
        })
        .child(
            ghost_icon(
                "reviews-previous-change",
                IconName::ArrowUp,
                "Previous Change (P)",
            )
            .on_click(cx.listener(|desk, _, _, cx| {
                if let Some((state, _)) = desk.reviews.opened.as_ref().and_then(|o| o.diff.as_ref())
                {
                    state.update(cx, |state, cx| state.previous_change(cx));
                }
            })),
        )
        .child(
            ghost_icon(
                "reviews-next-change",
                IconName::ArrowDown,
                "Next Change (N)",
            )
            .on_click(cx.listener(|desk, _, _, cx| {
                if let Some((state, _)) = desk.reviews.opened.as_ref().and_then(|o| o.diff.as_ref())
                {
                    state.update(cx, |state, cx| state.next_change(cx));
                }
            })),
        );
    div()
        .debug_selector(|| "reviews-diff".into())
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .child(toolbar)
        .child(diff(opened, state, look, cx))
        .into_any_element()
}

fn diff(
    opened: &Opened,
    state: &Entity<gpui_kit::component::diff::DiffState>,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let handle = cx.weak_entity();
    let viewed: Rc<HashSet<String>> = Rc::new(
        opened
            .patch
            .data
            .iter()
            .flat_map(|patch| patch.loaded.files.iter())
            .map(|file| file.path().to_string())
            .filter(|path| opened.is_viewed(path))
            .collect(),
    );
    let theme = &look.chrome.theme;
    let ink = Ink {
        card: look.hover,
        border: look.border,
        muted: from_hex(&look.chrome.muted),
        accent: from_hex(&theme.accent),
        warning: from_hex(&theme.warning),
        radius: look.skin.shape.kit_radius,
    };
    let slots = Rc::new(slots(opened));
    let annotations: Vec<DiffAnnotation> = slots.iter().map(|(slot, _)| slot.clone()).collect();
    let (header_handle, slot_handle, add_handle) = (handle.clone(), handle.clone(), handle);
    Diff::new(state)
        .flex_1()
        .min_h_0()
        .hover_highlight(DiffHoverHighlight::Both)
        .change_indicator(DiffChangeIndicator::Bars)
        .render_header_suffix(move |file, _, _| {
            let path = file.path().to_string();
            let viewed = viewed.contains(&path);
            let handle = header_handle.clone();
            Button::new(SharedString::from(format!("reviews-viewed-{path}")))
                .label("Viewed")
                .when(viewed, |button| button.icon(IconName::Check))
                .ghost()
                .xsmall()
                .selected(viewed)
                .on_click(move |_, _, cx| {
                    let _ = handle.update(cx, |desk, cx| desk.toggle_review_viewed(&path, cx));
                })
                .into_any_element()
        })
        .annotations(annotations)
        .render_annotation(move |annotation, _, _| {
            let slot = slots.iter().find(|(each, _)| each.id() == annotation.id());
            match slot.map(|(_, slot)| slot.clone()) {
                Some(Slot::Thread {
                    thread,
                    expanded,
                    reply,
                    busy,
                }) => thread_card(&thread, expanded, reply, busy, ink, slot_handle.clone()),
                Some(Slot::Pending(id, label, body)) => {
                    pending_card(id, label, body, ink, slot_handle.clone())
                }
                Some(Slot::Draft(input, label)) => {
                    draft_card(input, label, ink, slot_handle.clone())
                }
                None => div().into_any_element(),
            }
        })
        .on_add_annotation(move |range, window, cx| {
            let range = range.clone();
            let _ = add_handle.update(cx, |desk, cx| desk.start_review_comment(range, window, cx));
        })
}
