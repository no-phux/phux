//! One native input group with a stable Send slot and contextual controls.

use super::*;
use gpui_kit::component::input::{InputGroup, InputGroupAddon, InputGroupAddonAlignment};

pub(super) fn view(
    placement: &str,
    input: &Entity<TextareaState>,
    editable: bool,
    hint: Option<&'static str>,
    look: &Look,
    actions: impl Iterator<Item = AnyElement>,
) -> impl IntoElement {
    InputGroup::new(SharedString::from(format!("conversation-composer-{placement}")))
        .aria_label("Reply to agent")
        .rounded(corner(look, 12.))
        .input(Textarea::new(input)
            .appearance(false).aria_label("Message")
            .text_size(px(15.)).line_height(gpui::relative(1.45))
            .p(px(10.)).w_full().disabled(!editable))
        .addon(InputGroupAddon::new("conversation-actions")
            .align(InputGroupAddonAlignment::BlockEnd)
            .gap(px(6.))
            .child(div().flex_1().min_w_0().children(hint.map(|hint| {
                div().id("conversation-composer-hint")
                    .debug_selector(|| "conversation-composer-hint".into())
                    .text_xs().line_height(px(16.))
                    .text_color(from_hex(&look.chrome.muted))
                    .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(
                        "Provider requests must be answered before continuing. The first response across viewers wins. Read-only controls never fall back to terminal input."
                    ).build(window, cx))
                    .child(hint)
            })))
            .children(actions))
}

pub(super) fn actions(
    placement: &str,
    pending: bool,
    writable: bool,
    empty: bool,
    stopping: bool,
    cx: &mut Context<Desk>,
) -> impl Iterator<Item = AnyElement> + use<> {
    let stop_id = placement.to_owned();
    let send_id = placement.to_owned();
    [
        stopping.then(|| {
            div()
                .debug_selector(|| "conversation-stop".into())
                .flex_shrink_0()
                .child(
                    Button::new(SharedString::from(format!("conversation-stop-{placement}")))
                        .icon(IconName::Square)
                        .accessibility_label("Stop agent")
                        .ghost()
                        .small()
                        .tooltip("Stop agent · Esc")
                        .on_click(
                            cx.listener(move |desk, _, _, cx| desk.stop_conversation(&stop_id, cx)),
                        ),
                )
                .into_any_element()
        }),
        Some(
            div()
                .debug_selector(|| "conversation-send".into())
                .flex_shrink_0()
                .child(
                    Button::new(SharedString::from(format!("conversation-send-{placement}")))
                        .label(if pending { "Sending…" } else { "Send" })
                        .icon(IconName::ArrowUp)
                        .loading(pending)
                        .primary()
                        .small()
                        .w(px(72.))
                        .tooltip("Send · Enter. Shift+Enter adds a line.")
                        .disabled(!writable || empty)
                        .on_click(
                            cx.listener(move |desk, _, _, cx| desk.send_conversation(&send_id, cx)),
                        ),
                )
                .into_any_element(),
        ),
    ]
    .into_iter()
    .flatten()
}
