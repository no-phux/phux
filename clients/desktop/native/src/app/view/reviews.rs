//! The Reviews surface: every open pull request that wants you on the left,
//! the chosen one in full beside it. It takes the panes' place while open;
//! the panes keep running underneath.
//!
//! Keys when no field has them: `j`/`k` next and previous pull request, `n`/`p`
//! next and previous change, `1` `2` `3` the tabs, `c` the summary, `r`
//! refresh, `o` open on GitHub, Escape close.
//!
//! | Module | Owns |
//! |---|---|
//! | [`inbox`] | the list, its filter, and each pull request's row |
//! | [`detail`] | the opened pull request's header, merge controls, tabs and review bar |
//! | [`files`] | the file list and the diff |
//! | [`threads`] | threads, kept comments and the draft under the diff's lines |
//! | [`side`] | the conversation and checks tabs |

mod detail;
mod files;
mod inbox;
mod side;
mod threads;

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, Hsla, Window, div, px};

use crate::git::github::CheckState;

use super::super::color::from_hex;
use super::super::desk::Desk;
use super::super::skin::{Look, Surface, Surfaced};
use super::super::ui::muted;

/// The inbox column's width, and the share of a narrow window it takes at most.
const INBOX_WIDTH: f32 = 340.;
const INBOX_SHARE: f32 = 0.26;

/// The inbox column's width in a `viewport` points wide.
fn inbox_width(viewport: f32) -> f32 {
    INBOX_WIDTH.min(viewport * INBOX_SHARE)
}

pub(super) fn build(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    desk.ensure_review_summary(window, cx);
    let look = desk.look();
    let gutter = look.skin.shape.gutter;
    let focus = desk.reviews.focus.clone();
    let surface = div()
        .id("reviews")
        .debug_selector(|| "reviews-surface".into())
        .size_full()
        .flex()
        .overflow_hidden()
        .surface(&look, Surface::Pane)
        .when(gutter > 0., |panel| panel.border_color(look.border))
        .child(inbox::column(
            desk,
            inbox_width(window.viewport_size().width.as_f32()),
            &look,
            cx,
        ))
        .child(detail::column(desk, window, &look, cx));
    let surface = match focus {
        Some(focus) => surface.track_focus(&focus),
        None => surface,
    };
    div()
        .size_full()
        .pt(px(super::panes::top_inset(desk, &look)))
        .px(px(gutter))
        .pb(px(gutter))
        .child(surface)
        .into_any_element()
}

/// A centered title and detail where content would be.
fn placeholder(title: &str, detail: &str, look: &Look) -> AnyElement {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(4.))
        .px(px(24.))
        .child(
            div()
                .w_full()
                .text_center()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(from_hex(&look.chrome.subtext))
                .child(title.to_owned()),
        )
        .when(!detail.is_empty(), |column| {
            column.child(muted(look, detail.to_owned()).w_full().text_center())
        })
        .into_any_element()
}

/// The color a check state paints its dot.
fn check_color(state: Option<CheckState>, look: &Look) -> Hsla {
    let theme = &look.chrome.theme;
    match state {
        Some(CheckState::Success) => from_hex(&theme.success),
        Some(CheckState::Failure) => from_hex(&theme.danger),
        Some(CheckState::Pending) => from_hex(&theme.warning),
        _ => from_hex(&look.chrome.faint),
    }
}

/// `+N −M` in the theme's add and remove colors.
fn line_counts(added: u64, removed: u64, look: &Look) -> gpui::Div {
    let theme = &look.chrome.theme;
    div()
        .flex()
        .flex_shrink_0()
        .gap(px(4.))
        .text_xs()
        .child(
            div()
                .text_color(from_hex(&theme.success))
                .child(format!("+{added}")),
        )
        .child(
            div()
                .text_color(from_hex(&theme.danger))
                .child(format!("−{removed}")),
        )
}

/// A small chip in a theme color.
fn chip(text: &str, color: &str, look: &Look) -> gpui::Div {
    let tint = from_hex(color);
    div()
        .flex_shrink_0()
        .px(px(6.))
        .rounded(px(look.skin.shape.kit_radius))
        .bg(tint.opacity(0.14))
        .text_xs()
        .text_color(tint)
        .child(text.to_owned())
}

/// How long ago an RFC 3339 time was, as the rows say it.
fn ago(at: &str) -> String {
    let now = crate::connection::now_ms();
    super::super::reviews::iso_ms(at)
        .map_or_else(String::new, |then| super::super::ui::ago(then, now))
}
