//! Conversation identity, lifecycle badge, and the terminal handoff control.

use super::*;

pub(super) fn header(
    placement: &str,
    style: &Style<'_>,
    swatch: AgentState,
    state: &str,
    cx: &mut Context<Desk>,
) -> gpui::Div {
    let look = style.look;
    let terminal_id = placement.to_owned();
    div()
        .debug_selector(|| "conversation-header".into())
        .flex()
        .items_center()
        .gap(px(8.))
        .min_h(px(28.))
        .child(harness_mark(style.harness, Some(swatch), 16., look))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div().flex().items_center().gap(px(8.)).child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(look.ink)
                            .truncate()
                            .child(style.provider.to_owned()),
                    ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_xs()
                        .text_color(from_hex(&look.chrome.muted))
                        .child(state.to_owned()),
                ),
        )
        .child(
            Button::new(SharedString::from(format!(
                "conversation-terminal-{placement}"
            )))
            .icon(IconName::SquareTerminal)
            .accessibility_label("Show agent terminal")
            .ghost()
            .xsmall()
            .tooltip("Show this agent's terminal")
            .on_click(cx.listener(move |desk, _, window, cx| {
                desk.toggle_conversation(&terminal_id, window, cx)
            })),
        )
}
