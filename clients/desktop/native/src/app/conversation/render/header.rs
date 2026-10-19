//! Conversation identity, lifecycle badge, and the terminal handoff control.

use super::*;
use crate::app::ui::{badge, dot, pill};

pub(super) fn header(
    placement: &str,
    style: &Style<'_>,
    swatch: AgentState,
    state: &str,
    native: bool,
    cx: &mut Context<Desk>,
) -> gpui::Div {
    let (look, colors) = (style.look, style.colors);
    let terminal_id = placement.to_owned();
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(harness_mark(style.harness, Some(swatch), 18., look))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(look.ink)
                                .truncate()
                                .child(style.provider.to_owned()),
                        )
                        .child(if native {
                            badge("ACP", look)
                        } else {
                            pill(
                                "Attached TUI",
                                &format!("{}14", colors.theme.foreground),
                                &colors.muted,
                                look,
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_xs()
                        .text_color(from_hex(&colors.muted))
                        .child(dot(6., from_hex(agent_swatch(swatch))))
                        .child(state.to_owned()),
                ),
        )
        .child(
            Button::new(SharedString::from(format!(
                "conversation-terminal-{placement}"
            )))
            .label("Terminal")
            .icon(IconName::SquareTerminal)
            .ghost()
            .small()
            .tooltip("Show this agent's terminal")
            .on_click(cx.listener(move |desk, _, window, cx| {
                desk.toggle_conversation(&terminal_id, window, cx)
            })),
        )
}
