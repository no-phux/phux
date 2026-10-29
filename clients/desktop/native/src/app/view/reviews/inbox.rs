//! The inbox: open pull requests that want you, grouped by repository, newest
//! activity first, each with its checks, size and what stands in its way.

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, IntoElement, SharedString, div, px};
use gpui_kit::assets::IconName;
use gpui_kit::component::Sizable;

use crate::git::github::{InboxPull, Mergeability, ReviewDecision};

use super::super::super::color::from_hex;
use super::super::super::desk::Desk;
use super::super::super::reviews::{Filter, Key};
use super::super::super::skin::Look;
use super::super::super::ui::segmented::{Segment, segmented};
use super::super::super::ui::{dot, ghost_icon, muted, section_label};
use super::{ago, check_color, chip, line_counts, placeholder};

pub(super) fn column(desk: &Desk, width: f32, look: &Look, cx: &mut Context<Desk>) -> AnyElement {
    let reviews = &desk.reviews;
    let body = match (&reviews.inbox.data, &reviews.inbox.error) {
        (None, Some(error)) => placeholder("Could not read GitHub", error, look),
        (None, None) => placeholder("Reading GitHub…", "", look),
        (Some(_), _) if reviews.shown().is_empty() => placeholder(
            "Nothing waiting",
            "No open pull request matches this filter.",
            look,
        ),
        (Some(_), _) => list(desk, look, cx),
    };
    div()
        .debug_selector(|| "reviews-inbox".into())
        .w(px(width))
        .flex_shrink_0()
        .h_full()
        .flex()
        .flex_col()
        .border_r_1()
        .border_color(look.divider)
        .child(header(desk, look, cx))
        .child(filters(desk, look, cx))
        .children(
            reviews
                .inbox
                .error
                .as_ref()
                .filter(|_| reviews.inbox.data.is_some())
                .map(|error| {
                    muted(look, format!("Last refresh failed: {error}"))
                        .px(px(12.))
                        .pb(px(6.))
                        .text_color(from_hex(&look.chrome.theme.danger))
                }),
        )
        .child(body)
        .into_any_element()
}

fn header(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let count = desk.reviews.shown().len();
    let loading = desk.reviews.inbox.loading;
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(6.))
        .pl(px(14.))
        .pr(px(8.))
        .py(px(8.))
        .child(
            gpui_kit::component::Icon::new(IconName::GitPullRequest)
                .xsmall()
                .text_color(from_hex(&look.chrome.muted)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(if count == 0 {
                    "Reviews".to_owned()
                } else {
                    format!("Reviews  {count}")
                }),
        )
        .when(loading, |header| header.child(muted(look, "Refreshing…")))
        .child(
            ghost_icon("reviews-refresh", IconName::RefreshCw, "Refresh (R)").on_click(
                cx.listener(|desk, _, _, cx| {
                    desk.refresh_inbox(cx);
                    cx.notify();
                }),
            ),
        )
        .child(
            ghost_icon("reviews-close", IconName::Close, "Close Reviews")
                .on_click(cx.listener(|desk, _, window, cx| desk.execute("reviews", window, cx))),
        )
}

fn filters(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let segment = |value, label, selector: &str| Segment {
        value,
        label,
        icon: None,
        selector: selector.to_owned(),
    };
    let handle = cx.weak_entity();
    div()
        .flex_shrink_0()
        .px(px(10.))
        .pb(px(8.))
        .child(segmented(
            look,
            desk.reviews.filter,
            [
                segment(Filter::All, "All", "reviews-filter-all"),
                segment(Filter::Mine, "Mine", "reviews-filter-mine"),
                segment(Filter::Requested, "Requested", "reviews-filter-requested"),
                segment(Filter::Others, "Others", "reviews-filter-others"),
            ],
            move |filter| {
                let handle = handle.clone();
                move |_, _, cx| {
                    let _ = handle.update(cx, |desk, cx| desk.set_reviews_filter(filter, cx));
                }
            },
        ))
}

fn list(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> AnyElement {
    let groups = desk.reviews.groups();
    let truncated = desk
        .reviews
        .inbox
        .data
        .as_ref()
        .is_some_and(|inbox| inbox.truncated);
    div()
        .id("reviews-inbox-list")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(6.))
        .pb(px(8.))
        .children(groups.into_iter().map(|(repo, pulls)| {
            div()
                .flex()
                .flex_col()
                .gap(px(1.))
                .pt(px(6.))
                .child(section_label(&repo, look).px(px(8.)))
                .children(pulls.into_iter().map(|pull| row(desk, pull, look, cx)))
        }))
        .when(truncated, |list| {
            list.child(
                muted(look, "Showing the 100 most recent of each search.")
                    .px(px(8.))
                    .pt(px(8.)),
            )
        })
        .into_any_element()
}

/// What stands in the pull request's way, as chips.
fn chips(pull: &InboxPull, look: &Look) -> Vec<gpui::Div> {
    let theme = &look.chrome.theme;
    let mut chips = Vec::new();
    if pull.is_draft {
        chips.push(chip("Draft", &look.chrome.muted, look));
    }
    if pull.mergeable == Mergeability::Conflicting {
        chips.push(chip("Conflicts", &theme.danger, look));
    }
    match pull.review_decision {
        Some(ReviewDecision::Approved) => chips.push(chip("Approved", &theme.success, look)),
        Some(ReviewDecision::ChangesRequested) => chips.push(chip("Changes", &theme.warning, look)),
        _ => {}
    }
    if pull.reasons.requested {
        chips.push(chip("Review", &theme.accent, look));
    }
    if pull.unresolved_threads > 0 {
        chips.push(chip(
            &format!("{} open", pull.unresolved_threads),
            &theme.warning,
            look,
        ));
    }
    chips
}

fn row(desk: &Desk, pull: &InboxPull, look: &Look, cx: &mut Context<Desk>) -> AnyElement {
    let key = Key::of(pull);
    let chips = chips(pull, look);
    let opened = desk.reviews.opened_key() == Some(&key);
    let cursor = desk.reviews.cursor.as_ref() == Some(&key);
    let id = SharedString::from(format!("reviews-row-{}-{}", pull.repo, pull.number));
    let selector = id.to_string();
    let detail = format!(
        "#{} · {} · {}",
        pull.number,
        pull.author,
        ago(&pull.updated_at)
    );
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex()
        .gap(px(8.))
        .px(px(8.))
        .py(px(6.))
        .rounded(px(look.skin.shape.kit_radius))
        .cursor_pointer()
        .hover(|row| row.bg(look.hover))
        .when(opened || cursor, |row| row.bg(look.active))
        .on_click(cx.listener(move |desk, _, _, cx| desk.open_pull(key.clone(), cx)))
        .child(
            div()
                .pt(px(5.))
                .flex_shrink_0()
                .child(dot(8., check_color(pull.checks, look))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .truncate()
                        .text_size(px(13.))
                        .font_weight(if pull.is_draft {
                            FontWeight::NORMAL
                        } else {
                            FontWeight::MEDIUM
                        })
                        .text_color(if pull.is_draft {
                            from_hex(&look.chrome.muted)
                        } else {
                            look.ink
                        })
                        .child(pull.title.clone()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(muted(look, detail).flex_1().min_w_0().truncate())
                        .child(line_counts(pull.additions, pull.deletions, look)),
                )
                .when(!chips.is_empty(), |cell| {
                    cell.child(div().flex().flex_wrap().gap(px(4.)).children(chips))
                }),
        )
        .into_any_element()
}
