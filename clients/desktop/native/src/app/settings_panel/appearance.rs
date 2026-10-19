//! Theme and font selection, skins, and their live previews.

use gpui::prelude::*;
use gpui::{AnyElement, App, Hsla, SharedString, WeakEntity, div, px, relative};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::select::Select;
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::{ActiveTheme, IconName, Sizable};

use crate::settings::theme::{self, Palette};
use crate::settings::{self, DisplayPrefs, Skin};

use super::super::color::from_hex;
use super::super::desk::Desk;
use super::super::skin::Look;
use super::super::ui::muted;
use super::{click_sets, look_of, notes, prefs_of, resets, sourced, stepper, toggle};

fn font_status(item: SettingItem, desk: &Desk, cx: &App) -> SettingItem {
    let mut font = gpui::font(desk.prefs.font_family.clone());
    crate::fonts::resolve(&mut font, cx);
    let source = sourced(desk, "", &["font-family"], |prefs| {
        prefs.font_family.clone()
    });
    if font.family.as_ref() == desk.prefs.font_family {
        item.description(notes([
            source.as_str(),
            "Fonts installed after launch are listed after a restart.",
        ]))
    } else {
        item.description(notes([
            &format!(
                "{} is unavailable. Using Paper Mono.",
                desk.prefs.font_family
            ),
            source.as_str(),
        ]))
    }
}

/// The theme's source. A Ghostty theme reads the config's colours live, so a
/// file edit reaches it on reload rather than on a fresh import.
fn theme_source(desk: &Desk) -> String {
    let source =
        settings::display_provenance(&desk.prefs, desk.ghostty(), &["background"], |prefs| {
            prefs.theme_id.clone()
        });
    let applies = match source {
        settings::Provenance::Ghostty(_) => settings::Applies::OnReload,
        _ => source.applies(),
    };
    notes([source.label().as_str(), applies.label()])
}

pub(super) fn appearance_page(desk: &Desk, handle: &WeakEntity<Desk>, cx: &App) -> SettingPage {
    let mut themes = theme::themes();
    if let Some(extra) = settings::ghostty_theme(desk.ghostty()) {
        themes.insert(0, extra);
    }
    let theme_handle = handle.clone();
    let theme_item = SettingItem::new(
        "Theme",
        SettingField::render(move |_, _, cx| {
            let selected = prefs_of(&theme_handle, cx).theme_id;
            let label = themes
                .iter()
                .find(|theme| theme.id == selected)
                .map_or_else(|| selected.clone(), |theme| theme.name.clone());
            let choices: Vec<_> = themes
                .iter()
                .map(|theme| (theme.id.clone(), theme.name.clone()))
                .collect();
            let writer = theme_handle.clone();
            div().debug_selector(|| "settings-theme".into()).child(
                Button::new("settings-theme")
                    .label(label)
                    .outline()
                    .small()
                    .w(px(260.))
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, _, _| {
                        choices.iter().fold(menu, |menu, (id, name)| {
                            let id = id.clone();
                            menu.item(
                                PopupMenuItem::new(name.clone())
                                    .checked(id == selected)
                                    .on_click(click_sets(&writer, move |prefs| {
                                        prefs.theme_id.clone_from(&id)
                                    })),
                            )
                        })
                    }),
            )
        }),
    )
    .description(theme_source(desk))
    .keywords(["theme", "colors", "dark", "light"]);
    let mut font_group = SettingGroup::new();
    if let Some(controls) = desk.settings_controls.as_ref() {
        let font = controls.font.clone();
        let picker = SettingField::render(move |_, _, _| {
            Select::new(&font)
                .id("font-family")
                .accessibility_label("Terminal font family")
                .w(px(260.))
                .rounded(px(10.))
                .menu_width(px(340.))
                .menu_max_h(px(360.))
                .search_placeholder("Find a font family…")
                .empty(|_, cx| {
                    div()
                        .p(px(20.))
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(div().text_sm().child("No matching fonts"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Try another family name."),
                        )
                })
        });
        let item = SettingItem::new(
            "Font family",
            resets(picker, handle, |prefs| &mut prefs.font_family),
        )
        .keywords(["font", "typeface", "monospace", "nerd", "glyphs"]);
        font_group = font_group.item(font_status(item, desk, cx));
    }
    SettingPage::new("Appearance")
        .icon(IconName::Palette)
        .group(
            font_group
                .item(stepper(
                    handle,
                    "Font size",
                    (9., 28., 1.),
                    |prefs| &mut prefs.font_size,
                    sourced(desk, "Points", &["font-size"], |prefs| {
                        prefs.font_size.to_string()
                    }),
                ))
                .item(theme_item),
        )
        .group(look_group(desk, handle))
}

/// Skin cards, the plain terminal switch, and motion.
fn look_group(desk: &Desk, handle: &WeakEntity<Desk>) -> SettingGroup {
    let plain_chord = desk
        .shown_chord("plain-terminal")
        .map(|chord| format!(" Toggle it with {chord}."))
        .unwrap_or_default();
    let native_chord = desk
        .shown_chord("native-agents")
        .map(|chord| format!(" Toggle it with {chord}."))
        .unwrap_or_default();
    let cards = handle.clone();
    SettingGroup::new()
        .title("Look")
        .item(
            SettingItem::render(move |_, _, cx| skin_cards(&cards, cx))
                .keywords(["skin", "look", "glass", "terminal", "plain", "chrome"]),
        )
        .item(
            toggle(
                handle,
                "Plain terminal",
                |prefs| &mut prefs.plain,
                format!("Opaque and square, with no sidebar or status bar.{plain_chord}"),
            )
            .keywords(["plain", "ghostty", "minimal", "terminal", "skin"]),
        )
        .item(
            toggle(
                handle,
                "Native agents",
                |prefs| &mut prefs.native_agents,
                format!(
                    "Agent panes open as conversations instead of terminals; each pane can still switch back.{native_chord}"
                ),
            )
            .keywords(["native", "agent", "conversation", "chat", "acp", "transcript"]),
        )
        .item(
            toggle(
                handle,
                "Animations",
                |prefs| &mut prefs.animations,
                "Pane folds, skin shifts and the laser cursor. macOS Reduce motion also turns them off.",
            )
            .keywords(["motion", "animation", "reduce"]),
        )
        .item(
            toggle(
                handle,
                "Laser cursor",
                |prefs| &mut prefs.cursor_laser,
                "A jumping cursor streaks a beam behind it.",
            )
            .keywords(["cursor", "laser", "trail"]),
        )
}

/// One card per skin, previewed in the current theme. Plain terminal
/// overrides the pick, so a hint says so and the other cards dim.
fn skin_cards(handle: &WeakEntity<Desk>, cx: &App) -> AnyElement {
    let prefs = prefs_of(handle, cx);
    let look = look_of(handle, cx);
    let ink = look.ink;
    let picked = settings::skin_by_id(&prefs.skin_id).id;
    let hint = prefs.plain.then(|| {
        div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(10.))
            .child(muted(
                &look,
                "Plain terminal is on. Your skin returns when it is off.",
            ))
            .child(
                Button::new("skin-plain-off")
                    .label("Turn off")
                    .xsmall()
                    .ghost()
                    .on_click(click_sets(handle, |prefs| prefs.plain = false)),
            )
    });
    let card = |skin: &'static Skin| {
        let selected = skin.id == picked;
        let worn = if prefs.plain {
            skin.id == settings::skin::TERMINAL.id
        } else {
            selected
        };
        div()
            .id(SharedString::from(format!("skin-card-{}", skin.id)))
            .debug_selector(move || format!("skin-card-{}", skin.id))
            .w(px(168.))
            .p(px(4.))
            .rounded(px(10.))
            .border_1()
            .border_color(if selected {
                ink.opacity(0.35)
            } else {
                gpui::transparent_black()
            })
            .hover(|card| card.bg(ink.opacity(0.04)))
            .cursor_pointer()
            .flex()
            .flex_col()
            .gap(px(6.))
            .when(!worn, |card| card.opacity(0.7))
            .child(skin_preview(skin, &look.chrome, &prefs))
            .child(
                div()
                    .px(px(2.))
                    .pb(px(2.))
                    .text_sm()
                    .text_color(ink.opacity(if selected { 1. } else { 0.65 }))
                    .child(skin.name),
            )
            .on_click(click_sets(handle, move |prefs| {
                prefs.skin_id = skin.id.to_owned()
            }))
    };
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .children(hint)
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(10.))
                .children(settings::skins().iter().map(card)),
        )
        .into_any_element()
}

/// A window in miniature, painted from the skin's own tokens: sheet,
/// titlebar, sidebar, gutter, pane cards and status bar.
fn skin_preview(skin: &Skin, colors: &Palette, prefs: &DisplayPrefs) -> AnyElement {
    let look = Look::of(skin, colors, prefs);
    let (chrome, shape) = (&skin.chrome, &skin.shape);
    let scaled = |value: f32| px(value * 0.3);
    let corner = px(shape.radius * 0.4);
    let bar = |width: f32, color: Hsla| div().h(px(2.)).w(relative(width)).rounded_full().bg(color);
    let ink = look.ink.opacity(0.7);
    let muted = from_hex(&colors.muted);
    let accent = from_hex(&colors.theme.accent);
    let pane = |lines: [(f32, Hsla); 3]| {
        div()
            .flex_1()
            .h_full()
            .rounded(corner)
            .bg(look.card)
            .flex()
            .flex_col()
            .gap(px(3.))
            .p(px(if shape.gutter > 0. { 4. } else { 3. }))
            .children(lines.map(|(width, color)| bar(width, color)))
    };
    let titlebar = div()
        .h(scaled(chrome.titlebar))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(5.))
        .child(
            div()
                .flex()
                .gap(px(2.))
                .children((0..3).map(|_| div().size(px(3.)).rounded_full().bg(muted))),
        )
        .when(chrome.tabs == settings::TabsWhen::Always, |bar| {
            bar.child(
                div()
                    .h(px(6.))
                    .w(px(28.))
                    .rounded(scaled(shape.tab_radius))
                    .bg(look.active),
            )
        });
    let sidebar = (chrome.sidebar > 0.).then(|| {
        div()
            .w(px(30. * chrome.sidebar))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(4.))
            .pt(px(2.))
            .child(bar(0.8, look.active))
            .child(bar(0.6, look.hover))
            .child(bar(0.7, look.hover))
    });
    let split = (shape.divider > 0.).then(|| div().w(px(shape.divider)).h_full().bg(look.divider));
    let body = div()
        .flex_1()
        .min_h_0()
        .flex()
        .gap(scaled(shape.gap))
        .px(scaled(shape.gutter))
        .pb(scaled(if chrome.status > 0. { 0. } else { shape.gutter }))
        .children(sidebar)
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .gap(scaled(shape.gap))
                .child(pane([(0.5, accent), (0.85, ink), (0.6, muted)]))
                .children(split)
                .child(pane([(0.7, ink), (0.4, muted), (0.9, ink)])),
        );
    div()
        .w_full()
        .h(px(88.))
        .rounded(px(6.))
        .overflow_hidden()
        .border_1()
        .border_color(look.border)
        .bg(look.sheet)
        .flex()
        .flex_col()
        .child(titlebar)
        .child(body)
        .when(chrome.status > 0., |preview| {
            preview.child(div().h(scaled(chrome.status)).flex_shrink_0())
        })
        .into_any_element()
}
