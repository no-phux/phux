//! The Conversation tab (the description, comments and verdicts, oldest
//! first) and the Checks tab (every check on the head, a failed job's log).

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, IntoElement, SharedString, div, px};
use gpui_kit::assets::IconName;
use gpui_kit::component::Selectable;
use gpui_kit::component::Sizable;
use gpui_kit::component::text::TextView;

use crate::git::github::{CheckState, Entry, Pull};

use super::super::super::color::from_hex;
use super::super::super::desk::Desk;
use super::super::super::reviews::Opened;
use super::super::super::skin::Look;
use super::super::super::ui::{dot, ghost_button, ghost_icon, muted};
use super::{ago, check_color, chip, placeholder};

fn markdown(id: String, text: &str) -> TextView {
    TextView::markdown(SharedString::from(id), text.to_owned())
        .selectable(true)
        .scrollable(false)
}

fn verdict(state: &str, look: &Look) -> Option<gpui::Div> {
    let theme = &look.chrome.theme;
    match state {
        "APPROVED" => Some(chip("Approved", &theme.success, look)),
        "CHANGES_REQUESTED" => Some(chip("Changes requested", &theme.warning, look)),
        "DISMISSED" => Some(chip("Dismissed", &look.chrome.muted, look)),
        _ => None,
    }
}

pub(super) fn conversation(_opened: &Opened, pull: &Pull, look: &Look) -> AnyElement {
    let entry = |entry: &Entry| {
        let note = entry.note();
        let state = match entry {
            Entry::Review { state, .. } => verdict(state, look),
            Entry::Comment(_) => None,
        };
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .p(px(10.))
            .rounded(px(look.skin.shape.kit_radius))
            .bg(look.hover)
            .border_1()
            .border_color(look.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_xs()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(note.author.clone()),
                    )
                    .child(muted(look, ago(&note.created_at)))
                    .children(state),
            )
            .when(!note.body.trim().is_empty(), |card| {
                card.child(markdown(format!("review-entry-{}", note.id), &note.body))
            })
    };
    let body = if pull.body.trim().is_empty() {
        muted(look, "No description.").into_any_element()
    } else {
        markdown(format!("review-body-{}", pull.id), &pull.body).into_any_element()
    };
    div()
        .id("reviews-conversation")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(10.))
        .px(px(16.))
        .py(px(12.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(muted(
                    look,
                    format!("{} opened this {}", pull.author, ago(&pull.created_at)),
                ))
                .child(body),
        )
        .children(pull.conversation.iter().map(entry))
        .into_any_element()
}

pub(super) fn checks(
    opened: &Opened,
    pull: &Pull,
    font: SharedString,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    if pull.checks.is_empty() {
        return placeholder("No checks", "Nothing reported on the head commit.", look);
    }
    let mut ordered: Vec<_> = pull.checks.iter().collect();
    // Failures first, then running, then the rest, each by name.
    let rank = |state: CheckState| match state {
        CheckState::Failure => 0,
        CheckState::Pending => 1,
        _ => 2,
    };
    ordered.sort_by(|a, b| rank(a.state).cmp(&rank(b.state)).then(a.name.cmp(&b.name)));
    let log = opened.log.as_ref();
    div()
        .id("reviews-checks")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(2.))
        .px(px(10.))
        .py(px(8.))
        .children(ordered.into_iter().map(|check| {
            let shown = log.is_some_and(|log| Some(log.job) == check.job);
            let url = check.url.clone();
            let id = SharedString::from(format!(
                "reviews-check-{}-{}",
                check.name,
                check.job.unwrap_or_default()
            ));
            let row = div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(8.))
                .py(px(4.))
                .rounded(px(look.skin.shape.kit_radius))
                .hover(|row| row.bg(look.hover))
                .child(dot(8., check_color(Some(check.state), look)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(13.))
                        .child(match &check.workflow {
                            Some(workflow) => format!("{workflow} / {}", check.name),
                            None => check.name.clone(),
                        }),
                )
                .children(
                    check
                        .description
                        .clone()
                        .map(|text| muted(look, text).truncate()),
                )
                .children(
                    check
                        .job
                        .filter(|_| check.state == CheckState::Failure)
                        .map(|job| {
                            ghost_button(
                                SharedString::from(format!("{id}-log")),
                                if shown { "Hide Log" } else { "Log" },
                            )
                            .xsmall()
                            .selected(shown)
                            .on_click(cx.listener(move |desk, _, _, cx| desk.read_job_log(job, cx)))
                        }),
                )
                .children(url.filter(|url| url.starts_with("https://")).map(|url| {
                    ghost_icon(
                        SharedString::from(format!("{id}-open")),
                        IconName::ExternalLink,
                        "Open",
                    )
                    .on_click(move |_, _, cx| cx.open_url(&url))
                }));
            let text = log
                .filter(|_| shown)
                .map(|log| match (&log.text.data, &log.text.error) {
                    (Some(text), _) => text.clone(),
                    (None, Some(error)) => format!("Could not read the log: {error}"),
                    (None, None) => "Reading the log…".to_owned(),
                });
            div()
                .flex()
                .flex_col()
                .child(row)
                .children(text.map(|text| {
                    div()
                        .id(SharedString::from(format!("{id}-text")))
                        .mx(px(8.))
                        .mb(px(6.))
                        .p(px(8.))
                        .max_h(px(420.))
                        .overflow_y_scroll()
                        .rounded(px(look.skin.shape.kit_radius))
                        .bg(look.ink.opacity(0.05))
                        .font_family(font.clone())
                        .text_xs()
                        .text_color(from_hex(&look.chrome.subtext))
                        .whitespace_normal()
                        .children(text.lines().map(|line| {
                            let error = line.starts_with("##[error]");
                            div()
                                .when(error, |line| {
                                    line.text_color(from_hex(&look.chrome.theme.danger))
                                })
                                .child(if line.is_empty() {
                                    " ".to_owned()
                                } else {
                                    line.to_owned()
                                })
                        }))
                }))
        }))
        .into_any_element()
}
