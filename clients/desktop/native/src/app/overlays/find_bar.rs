//! The find bar over a pane: the field, the match count, and its controls.

use gpui::prelude::*;
use gpui::{AnyElement, Context, div, px};
use gpui_kit::component::input::Input;
use gpui_kit::component::{Icon, IconName, Sizable};

use super::{card, corner};
use crate::app::desk::Desk;
use crate::app::skin::Look;
use crate::app::ui::{ghost_icon, muted};

/// Room for the widest common count, `4096 of 4096+`, and `no matches`.
const STATUS_WIDTH: f32 = 112.;

/// The match count in a slot of fixed width. An empty count keeps its slot
/// and paints nothing, so Next, Previous and typing never move the field.
fn status(look: &Look, text: String) -> gpui::Div {
    let shown = !text.is_empty();
    muted(look, text)
        .debug_selector(|| "find-status".into())
        .flex_shrink_0()
        .w(px(STATUS_WIDTH))
        .px(px(6.))
        .py(px(3.))
        .text_center()
        .truncate()
        .rounded(corner(look, 5.))
        .when(shown, |pill| pill.bg(look.ink.opacity(0.045)))
}

/// The find bar a pane shows while it is searched. The field takes what the
/// card leaves after fixed-width controls, so its width never follows the count.
pub fn find_bar(desk: &Desk, case_sensitive: bool, cx: &mut Context<Desk>) -> AnyElement {
    let Some((input, _)) = desk.find_input.as_ref() else {
        return div().into_any_element();
    };
    let look = desk.look();
    card(&look, 420.)
        .absolute()
        .top(px(12.))
        .right(px(12.))
        .p(px(8.))
        .flex()
        .items_center()
        .gap(px(6.))
        .child(
            div()
                .debug_selector(|| "find-input".into())
                .flex_1()
                .min_w(px(0.))
                .child(
                    Input::new(input)
                        .appearance(false)
                        .prefix(Icon::new(IconName::Search).small()),
                ),
        )
        .child(status(&look, desk.find_status()))
        .child(
            ghost_icon("find-case", IconName::CaseSensitive, "Match case")
                .small()
                .toggled(case_sensitive)
                .on_click(cx.listener(|desk, _, window, cx| {
                    let case = !desk
                        .find
                        .as_ref()
                        .map(|find| find.case_sensitive)
                        .unwrap_or(false);
                    let query = desk
                        .find
                        .as_ref()
                        .map(|find| find.query.clone())
                        .unwrap_or_default();
                    desk.run_search(query, case, cx);
                    desk.after_command(window, cx);
                })),
        )
        .children(
            [
                ("find-prev", IconName::ChevronUp, "Previous match"),
                ("find-next", IconName::ChevronDown, "Next match"),
                ("find-close", IconName::Close, "Close"),
            ]
            .map(|(id, icon, tooltip)| {
                ghost_icon(id, icon, tooltip)
                    .small()
                    .on_click(cx.listener(move |desk, _, window, cx| desk.execute(id, window, cx)))
            }),
        )
        .into_any_element()
}
