//! The opened pull request: what it is and where it stands, merge and check
//! out, the three tabs, and the review bar that submits everything at once.

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, IntoElement, Window, div, px};
use gpui_kit::assets::IconName;
use gpui_kit::base::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::{Selectable, Sizable};

use crate::git::github::{CheckState, MergeState, Mergeability, Pull, ReviewDecision, ReviewEvent};

use super::super::super::color::from_hex;
use super::super::super::desk::Desk;
use super::super::super::reviews::{DraftFor, Opened, Tab};
use super::super::super::skin::Look;
use super::super::super::ui::segmented::{Segment, segmented};
use super::super::super::ui::{ghost_button, muted};
use super::{ago, check_color, chip, line_counts, placeholder};

pub(super) fn column(
    desk: &Desk,
    window: &mut Window,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let Some(opened) = desk.reviews.opened.as_ref() else {
        return placeholder(
            "Choose a pull request",
            "J and K move through the inbox · N and P through changes · O opens it on GitHub",
            look,
        );
    };
    let body = match (&opened.pull.data, &opened.pull.error) {
        (None, Some(error)) => placeholder("Could not read the pull request", error, look),
        (None, None) => placeholder(&format!("Reading {}…", opened.key.label()), "", look),
        (Some(pull), _) => div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(header(desk, opened, pull, look, cx))
            .children(opened.pull.error.as_ref().map(|error| {
                muted(look, format!("Refresh failed; showing the last result. {error}"))
                    .px(px(16.)).py(px(6.))
                    .text_color(from_hex(&look.chrome.theme.warning))
            }))
            .when(opened.revision_changed(), |column| {
                column.child(
                    muted(look, if opened.base_changed() {
                        "The base changed. Remove pending line comments and refresh to review the new target."
                    } else {
                        "New commits arrived. Your notes stay on the previous diff until submitted or removed."
                    })
                        .px(px(16.)).py(px(6.))
                        .text_color(from_hex(&look.chrome.theme.warning)),
                )
            })
            .child(tabs(opened, pull, look, cx))
            .child(match opened.tab {
                Tab::Files => super::files::files(desk, opened, window, look, cx),
                Tab::Conversation => super::side::conversation(opened, pull, look),
                Tab::Checks => super::side::checks(
                    opened,
                    pull,
                    desk.prefs.font_family.clone().into(),
                    look,
                    cx,
                ),
            })
            .child(review_bar(opened, pull, look, cx))
            .into_any_element(),
    };
    div()
        .debug_selector(|| "reviews-detail".into())
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .child(body)
        .into_any_element()
}

/// `1 check fails`, `3 checks fail`.
fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

/// Where the pull request stands, in a few words and one color.
fn standing(pull: &Pull, look: &Look) -> Option<(&'static str, String)> {
    let theme = &look.chrome.theme;
    let said = match (pull.mergeable, pull.merge_state) {
        (Mergeability::Conflicting, _) | (_, MergeState::Dirty) => {
            ("Conflicts with the base", theme.danger.clone())
        }
        (_, MergeState::Behind) => ("Behind the base", theme.warning.clone()),
        (_, MergeState::Blocked) => (
            "Blocked by required checks or reviews",
            theme.warning.clone(),
        ),
        (_, MergeState::Unstable) => ("Mergeable; some checks fail", theme.warning.clone()),
        (_, MergeState::Clean | MergeState::HasHooks) => ("Ready to merge", theme.success.clone()),
        (_, MergeState::Draft) => ("Draft", look.chrome.muted.clone()),
        _ => return None,
    };
    Some(said)
}

fn header(
    desk: &Desk,
    opened: &Opened,
    pull: &Pull,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let theme = &look.chrome.theme;
    let busy = opened.busy;
    let fork = pull.head_repo.as_deref().filter(|_| pull.is_fork());
    let head = fork.map_or_else(
        || pull.head_ref.clone(),
        |fork| format!("{fork}:{}", pull.head_ref),
    );
    let detail = format!(
        "{}#{} · {} · {head} → {} · updated {}",
        pull.repo,
        pull.number,
        pull.author,
        pull.base_ref,
        ago(&pull.updated_at)
    );
    let failing = pull
        .checks
        .iter()
        .filter(|c| c.state == CheckState::Failure)
        .count();
    let pending = pull
        .checks
        .iter()
        .filter(|c| c.state == CheckState::Pending)
        .count();
    let checks = match (failing, pending) {
        (0, 0) if pull.checks.is_empty() => None,
        (0, 0) => Some((
            plural(pull.checks.len(), "check passes", "checks pass"),
            theme.success.clone(),
        )),
        (0, pending) => Some((
            plural(pending, "check running", "checks running"),
            theme.warning.clone(),
        )),
        (failing, _) => Some((
            plural(failing, "check fails", "checks fail"),
            theme.danger.clone(),
        )),
    };
    let decision = match pull.review_decision {
        Some(ReviewDecision::Approved) => Some(("Approved", theme.success.clone())),
        Some(ReviewDecision::ChangesRequested) => {
            Some(("Changes requested", theme.warning.clone()))
        }
        Some(ReviewDecision::ReviewRequired) => {
            Some(("Review required", look.chrome.muted.clone()))
        }
        _ => None,
    };
    let checkout_label = if opened.place.worktree.is_some() {
        "Open Worktree"
    } else {
        "Check Out"
    };
    div()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(6.))
        .px(px(16.))
        .pt(px(12.))
        .pb(px(10.))
        .border_b_1()
        .border_color(look.divider)
        .child(
            div()
                .flex()
                .items_start()
                .gap(px(8.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(pull.title.clone()),
                )
                .child(
                    ghost_button("reviews-open-github", "GitHub")
                        .xsmall()
                        .icon(IconName::ExternalLink)
                        .tooltip("Open on GitHub (O)")
                        .on_click(cx.listener(|desk, _, _, cx| desk.open_pull_in_browser(cx))),
                )
                .child(
                    ghost_button("reviews-check-out", checkout_label)
                        .xsmall()
                        .icon(IconName::GitBranch)
                        .tooltip("Open the branch in a worktree and session of its own")
                        .disabled(busy.is_some() || desk.remote.is_some())
                        .on_click(
                            cx.listener(|desk, _, window, cx| desk.check_out_pull(window, cx)),
                        ),
                ),
        )
        .child(muted(look, detail).truncate())
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.))
                .child(line_counts(pull.additions, pull.deletions, look))
                .children(checks.map(|(text, color)| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .child(super::super::super::ui::dot(
                            7.,
                            check_color(pull.rollup, look),
                        ))
                        .child(muted(look, text).text_color(from_hex(&color)))
                }))
                .children(decision.map(|(text, color)| chip(text, &color, look)))
                .children(standing(pull, look).map(|(text, color)| chip(text, &color, look)))
                .children(opened.place.agent.as_ref().map(|agent| {
                    chip(
                        &format!("Agent: {}", agent.title),
                        &look.chrome.theme.accent,
                        look,
                    )
                }))
                .children(busy.map(|what| muted(look, format!("{what}…"))))
                .child(div().flex_1())
                .child(merge_controls(opened, pull, look, cx)),
        )
}

fn merge_blocked(opened: &Opened, pull: &Pull) -> bool {
    opened.patch.data.is_none()
        || opened.revision_changed()
        || pull.is_draft
        || pull.mergeable == Mergeability::Conflicting
        || matches!(
            pull.merge_state,
            MergeState::Dirty | MergeState::Blocked | MergeState::Behind
        )
}

fn merge_controls(
    opened: &Opened,
    pull: &Pull,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let busy = opened.busy.is_some();
    let method = opened.merge_method();
    let blocked = merge_blocked(opened, pull);
    let handle = cx.weak_entity();
    let methods = (pull.methods.allowed.len() > 1).then(|| {
        segmented(
            look,
            method.unwrap_or(pull.methods.preferred),
            pull.methods.allowed.iter().map(|method| Segment {
                value: *method,
                label: match method {
                    crate::git::github::MergeMethod::Merge => "Merge",
                    crate::git::github::MergeMethod::Squash => "Squash",
                    crate::git::github::MergeMethod::Rebase => "Rebase",
                },
                icon: None,
                selector: format!("reviews-method-{method:?}"),
            }),
            move |method| {
                let handle = handle.clone();
                move |_, _, cx| {
                    let _ = handle.update(cx, |desk, cx| desk.set_merge_method(method, cx));
                }
            },
        )
    });
    let label = match (opened.confirm_merge, method) {
        (true, Some(method)) => format!("Confirm {}", method.label().to_lowercase()),
        (false, Some(method)) => method.label().to_owned(),
        (_, None) => "Merge".to_owned(),
    };
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .when(pull.is_draft, |row| {
            row.child(
                ghost_button("reviews-ready", "Ready for Review")
                    .xsmall()
                    .disabled(busy)
                    .on_click(cx.listener(|desk, _, _, cx| desk.mark_ready(cx))),
            )
        })
        .children(methods)
        .when(
            pull.methods.auto_merge || pull.auto_merge.is_some(),
            |row| row.child(auto_merge_control(opened, pull, cx)),
        )
        .child({
            let button = Button::new("reviews-merge")
                .icon(IconName::GitMerge)
                .label(label)
                .xsmall()
                .disabled(busy || blocked || method.is_none())
                .on_click(cx.listener(|desk, _, _, cx| desk.merge_pull(cx)));
            if opened.confirm_merge {
                button.danger()
            } else {
                button.primary()
            }
        })
}

fn auto_merge_control(opened: &Opened, pull: &Pull, cx: &mut Context<Desk>) -> Button {
    let on = pull.auto_merge.is_some();
    Button::new("reviews-auto-merge")
        .label(if on { "Auto-merge On" } else { "Auto-merge" })
        .ghost()
        .xsmall()
        .selected(on)
        .tooltip("Auto-merge follows future eligible revisions when checks and reviews pass; it is not pinned to this diff. Click again to disable.")
        .disabled(opened.busy.is_some() || opened.auto_merge_change().is_none())
        .on_click(cx.listener(|desk, _, _, cx| desk.toggle_auto_merge(cx)))
}

fn tabs(opened: &Opened, pull: &Pull, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let failing = pull
        .checks
        .iter()
        .filter(|c| c.state == CheckState::Failure)
        .count();
    let open_threads = pull.threads.iter().filter(|t| !t.resolved).count();
    let (viewed, total) = opened.progress();
    let tab = |id: &'static str, label: String, tab: Tab| {
        Button::new(id)
            .label(label)
            .ghost()
            .xsmall()
            .selected(opened.tab == tab)
            .on_click(cx.listener(move |desk, _, _, cx| desk.set_review_tab(tab, cx)))
    };
    let files = if total > 0 {
        format!("Files {viewed}/{total}")
    } else {
        format!("Files {}", pull.files.len())
    };
    let threads = if open_threads > 0 {
        format!(" · {open_threads} open")
    } else {
        String::new()
    };
    let checks = if failing > 0 {
        format!("Checks {failing} failing")
    } else {
        format!("Checks {}", pull.checks.len())
    };
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(4.))
        .px(px(10.))
        .py(px(6.))
        .border_b_1()
        .border_color(look.divider)
        .child(tab("reviews-tab-files", files, Tab::Files))
        .child(tab(
            "reviews-tab-conversation",
            format!("Conversation {}{threads}", pull.conversation.len()),
            Tab::Conversation,
        ))
        .child(tab("reviews-tab-checks", checks, Tab::Checks))
}

/// Keep an unfinished composer accessible when its inline location is absent.
fn fallback_draft(opened: &Opened, look: &Look, cx: &mut Context<Desk>) -> AnyElement {
    let Some(draft) = opened.draft.as_ref().filter(|_| opened.draft_needs_bar()) else {
        return div().into_any_element();
    };
    let label = match draft.target {
        DraftFor::Reply(_) => "Post Reply",
        DraftFor::Lines(_) => "Add to Review",
    };
    div()
        .debug_selector(|| "reviews-fallback-draft".into())
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(muted(look, "Unfinished comment"))
        .child(
            Textarea::new(&draft.input)
                .aria_label("Unfinished comment")
                .w_full(),
        )
        .child(
            div()
                .flex()
                .gap(px(6.))
                .child(
                    ghost_button("reviews-fallback-cancel", "Cancel Comment")
                        .xsmall()
                        .on_click(cx.listener(|desk, _, _, cx| desk.cancel_review_draft(cx))),
                )
                .child(
                    ghost_button("reviews-fallback-save", label)
                        .xsmall()
                        .disabled(opened.busy.is_some())
                        .on_click(cx.listener(|desk, _, _, cx| desk.save_review_draft(cx))),
                ),
        )
        .into_any_element()
}

/// Kept comments, the summary, the verdict, and the submit button.
fn review_bar(
    opened: &Opened,
    pull: &Pull,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let pending = opened.pending.len();
    let busy = opened.busy.is_some();
    let own = pull.viewer_authored;
    let handle = cx.weak_entity();
    let event = segmented(
        look,
        opened.event,
        [
            (ReviewEvent::Comment, "Comment", "reviews-event-comment"),
            (ReviewEvent::Approve, "Approve", "reviews-event-approve"),
            (
                ReviewEvent::RequestChanges,
                "Request Changes",
                "reviews-event-changes",
            ),
        ]
        .into_iter()
        .map(|(value, label, selector)| Segment {
            value,
            label,
            icon: None,
            selector: selector.to_owned(),
        }),
        move |event| {
            let handle = handle.clone();
            move |_, _, cx| {
                let _ = handle.update(cx, |desk, cx| desk.set_review_event(event, cx));
            }
        },
    );
    let label = match pending {
        0 => "Submit Review".to_owned(),
        1 => "Submit with 1 Comment".to_owned(),
        n => format!("Submit with {n} Comments"),
    };
    let agent = opened.place.agent.as_ref();
    div()
        .debug_selector(|| "reviews-bar".into())
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(6.))
        .px(px(12.))
        .py(px(8.))
        .border_t_1()
        .border_color(look.divider)
        .child(fallback_draft(opened, look, cx))
        .children(opened.summary.as_ref().map(|(input, _)| {
            Textarea::new(input).aria_label("Review summary").w_full()
        }))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.))
                .when(!own, |row| row.child(event))
                .when(own, |row| {
                    row.child(muted(look, "Your own pull request: GitHub takes comments only."))
                })
                .child(div().flex_1())
                .children(agent.map(|agent| {
                    Button::new("reviews-to-agent")
                        .label(format!("Also paste into {}", agent.title))
                        .ghost()
                        .xsmall()
                        .when(opened.to_agent, |button| button.icon(IconName::Check))
                        .selected(opened.to_agent)
                        .tooltip("After GitHub takes the review, paste the notes into the agent working on this branch")
                        .on_click(cx.listener(|desk, _, _, cx| desk.toggle_review_to_agent(cx)))
                }))
                .when(agent.is_some(), |row| {
                    row.child(
                        ghost_button("reviews-agent-only", "Agent Only")
                            .xsmall()
                            .icon(IconName::Send)
                            .tooltip("Paste the notes into the agent without posting to GitHub")
                            .disabled(busy)
                            .on_click(cx.listener(|desk, _, _, cx| desk.send_review_to_agent(cx))),
                    )
                })
                .child(
                    Button::new("reviews-submit")
                        .label(label)
                        .primary()
                        .xsmall()
                        .disabled(busy)
                        .on_click(cx.listener(|desk, _, _, cx| desk.submit_review(cx))),
                ),
        )
}
