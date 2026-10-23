//! Search and edit exact native setting controls without leaving the palette.

use super::catalog::Item as SettingItem;
use super::{Page, catalog, content, native_settings, sync_controls};
use crate::app::desk::Desk;
use crate::app::overlays;
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, SharedString, WeakEntity, Window, div, px};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{IconName, Sizable};

/// Each page contributes its actual controls, including dynamic machine rows.
pub(crate) fn entries(
    desk: &Desk,
    handle: &WeakEntity<Desk>,
    cx: &App,
) -> Vec<(catalog::Key, SettingItem)> {
    Page::ALL
        .into_iter()
        .flat_map(|page| {
            content(desk, page, handle, desk.settings_controls.as_ref(), cx)
                .entries
                .into_iter()
                .filter(|(_, item)| item.actionable)
                .map(move |(group, item)| {
                    let key = catalog::Key {
                        page,
                        group,
                        title: item.title.clone(),
                    };
                    (key, item)
                })
        })
        .collect()
}

/// Edit one exact search result without leaving the palette or losing its query.
pub(crate) fn editor(
    desk: &mut Desk,
    key: &catalog::Key,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> AnyElement {
    sync_controls(desk, window, cx);
    let handle = cx.weak_entity();
    let item = entries(desk, &handle, cx)
        .into_iter()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, item)| item);
    let viewport = window.viewport_size();
    let width = (viewport.width.as_f32() - 32.).clamp(0., 720.);
    let page = gpui_kit::component::setting::SettingPage::new(key.title.clone()).group(
        gpui_kit::component::setting::SettingGroup::new()
            .items(item.map(|item| item.responsive_control(width < 720.))),
    );
    let settings = native_settings(SharedString::from(format!("palette-setting-{key:?}")), page);
    let look = desk.look();
    overlays::sheet(&look, width)
        .id("palette-panel")
        .debug_selector(|| "palette-panel".into())
        .h(px((viewport.height.as_f32() - 32.).clamp(0., 440.)))
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(overlays::launcher::hub_header(desk, &look.chrome, cx))
        .child(
            div()
                .debug_selector(|| "palette-settings-back".into())
                .w(px(180.))
                .h(px(40.))
                .flex_shrink_0()
                .px(px(14.))
                .flex()
                .items_center()
                .child(
                    Button::new("palette-settings-back")
                        .label("Back to results")
                        .icon(IconName::ChevronLeft)
                        .small()
                        .ghost()
                        .on_click(cx.listener(|desk, _, window, cx| {
                            desk.palette_view.edit = None;
                            if let Some(state) = desk.palette_state.clone() {
                                state.update(cx, |state, cx| state.focus(window, cx));
                            }
                            cx.notify();
                        })),
                ),
        )
        .child(
            div()
                .id("palette-setting-editor")
                .when_some(desk.palette_view.edit_focus.as_ref(), |editor, focus| {
                    editor.track_focus(focus)
                })
                .debug_selector(|| "palette-setting-editor".into())
                .flex_1()
                .min_h_0()
                .child(settings),
        )
        .into_any_element()
}
