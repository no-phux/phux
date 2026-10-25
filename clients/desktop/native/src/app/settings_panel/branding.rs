//! App-wide identity belongs to the process, independent of the originating desk.
use super::catalog::{Group, Item};
use crate::app::{branding, desk::Desk};
use gpui::prelude::*;
use gpui::{App, div, img, px};
use gpui_kit::component::Sizable;
use gpui_kit::component::button::Button;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::setting::SettingField;

pub(super) fn group(desk: &Desk, _cx: &App) -> Group {
    let state = desk.branding.clone();
    let field = SettingField::render(move |_, _, cx| {
        let model = state.read(cx);
        let selected = branding::profile(&model.id);
        let writer = state.clone();
        let id = selected.id.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .w(px(260.))
            .debug_selector(|| "settings-brand-profile".into())
            .child(
                img(format!("brand/{}.png", selected.id))
                    .w(px(64.))
                    .h(px(64.)),
            )
            .child(
                Button::new("settings-brand-profile-picker")
                    .label(selected.name.clone())
                    .outline()
                    .small()
                    .w_full()
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, _, _| {
                        branding::profiles().iter().fold(menu, |menu, profile| {
                            let target = writer.clone();
                            let next = profile.id.clone();
                            menu.item(
                                PopupMenuItem::new(profile.name.clone())
                                    .checked(profile.id == id)
                                    .on_click(move |_, _, cx| {
                                        target.update(cx, |state, cx| state.select(&next, cx))
                                    }),
                            )
                        })
                    }),
            )
            .children(
                model
                    .error
                    .clone()
                    .map(|error| div().text_sm().child(error)),
            )
    });
    let description = if cfg!(target_os = "macos") {
        "Changes app artwork and the Dock icon. Finder uses the release icon."
    } else {
        "Changes app artwork and the launcher icon. Your launcher may take a moment to refresh."
    };
    Group::new().title("Branding").item(
        Item::new("Brand profile", field)
            .wide()
            .description(description)
            .keywords(
                ["icon", "logo", "branding"]
                    .map(str::to_owned)
                    .into_iter()
                    .chain(branding::profiles().iter().map(|p| p.name.clone())),
            ),
    )
}
