//! Settings on Kit [`Settings`]: searchable pages of switches, sliders, number
//! fields and a font select. Every control writes its preference directly, so
//! Settings stays open, and every control moves both ways with a reset.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, Keystroke, SharedString, Subscription, WeakEntity, Window,
    div, px, relative,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::group_box::GroupBoxVariant;
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{ActiveTheme, Disableable, IconName, IndexPath, Sizable};
use phux_client_runtime::control::Status;

use crate::settings::theme::{self, Palette};
use crate::settings::{self, DisplayPrefs};

use super::color::from_hex;
use super::desk::Desk;
use super::keys::gpui_stroke;

type FontSelect = SelectState<SearchableVec<FontChoice>>;

/// Keep Kit's search, keyboard cursor and committed checkmark while previewing
/// each installed family in its own face.
#[derive(Clone)]
struct FontChoice(SharedString);

impl SelectItem for FontChoice {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.0.clone()
    }

    fn value(&self) -> &SharedString {
        &self.0
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut font = gpui::font(self.0.clone());
        crate::fonts::resolve(&mut font, cx);
        let bundled =
            self.0.as_ref() == crate::fonts::BUNDLED || self.0.as_ref() == crate::fonts::SYMBOLS;
        div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(12.))
            .py(px(5.))
            .child(
                div()
                    .w(px(42.))
                    .h(px(36.))
                    .flex_shrink_0()
                    .rounded(px(8.))
                    .bg(cx.theme().foreground.opacity(0.05))
                    .flex()
                    .items_center()
                    .justify_center()
                    .font_family(font.family)
                    .text_size(px(19.))
                    .child("Ag"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().text_sm().truncate().child(self.0.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if bundled {
                                "Bundled with Phux"
                            } else {
                                "Font family"
                            }),
                    ),
            )
    }
}

/// Slider and select state for one window's Settings. Built on first open.
pub(crate) struct SettingsControls {
    sliders: Vec<(Knob, Entity<SliderState>)>,
    font: Entity<FontSelect>,
    fonts: Vec<SharedString>,
    _subs: Vec<Subscription>,
}

/// A continuous preference, shown in display units (percent or multiplier).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Knob {
    LineHeight,
    CellWidth,
    Unfocused,
    Glass,
}

impl Knob {
    const ALL: [Knob; 4] = [
        Knob::LineHeight,
        Knob::CellWidth,
        Knob::Unfocused,
        Knob::Glass,
    ];

    /// Display range and step.
    fn range(self) -> (f32, f32, f32) {
        match self {
            Knob::LineHeight => (1., 2., 0.05),
            Knob::CellWidth => (50., 200., 1.),
            Knob::Unfocused => (15., 100., 5.),
            Knob::Glass => (55., 100., 5.),
        }
    }

    fn shown(self, prefs: &DisplayPrefs) -> f32 {
        match self {
            Knob::LineHeight => prefs.line_height as f32,
            Knob::CellWidth => (prefs.cell_width * 100.) as f32,
            Knob::Unfocused => (prefs.unfocused_opacity * 100.) as f32,
            Knob::Glass => (prefs.terminal_opacity * 100.) as f32,
        }
    }

    fn store(self, prefs: &mut DisplayPrefs, value: f32) {
        let value = f64::from(value);
        match self {
            Knob::LineHeight => prefs.line_height = value,
            Knob::CellWidth => prefs.cell_width = value / 100.,
            Knob::Unfocused => prefs.unfocused_opacity = value / 100.,
            Knob::Glass => prefs.terminal_opacity = value / 100.,
        }
    }

    fn readout(self, value: f32) -> String {
        match self {
            Knob::LineHeight => format!("{value:.2}"),
            _ => format!("{value:.0}%"),
        }
    }
}

/// Installed families, with the bundled default and current choice kept easy to find.
fn font_families(desk: &Desk, cx: &App) -> Vec<SharedString> {
    let mut installed = cx.text_system().all_font_names();
    installed.sort_unstable();
    installed.dedup();
    let ghostty = desk
        .ghostty()
        .font_family
        .as_ref()
        .filter(|family| installed.contains(family))
        .cloned();
    let mut families: Vec<SharedString> = Vec::new();
    let candidates = std::iter::once(crate::fonts::BUNDLED.to_owned())
        .chain(std::iter::once(crate::fonts::SYMBOLS.to_owned()))
        .chain(std::iter::once(desk.prefs.font_family.clone()))
        .chain(ghostty);
    for family in candidates {
        let family = family.trim();
        if !family.is_empty() && !families.iter().any(|known| known.as_ref() == family) {
            families.push(family.to_owned().into());
        }
    }
    let pinned = families.len();
    for family in installed {
        if !family.is_empty()
            && !families[..pinned]
                .iter()
                .any(|known| known.as_ref() == family)
        {
            families.push(family.into());
        }
    }
    families
}

/// Build the controls once, then bring every value in line with the current prefs.
pub(crate) fn prepare(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) {
    if desk.settings_controls.is_none() {
        let fonts = font_families(desk, cx);
        let mut subs = Vec::new();
        let mut sliders = Vec::new();
        for knob in Knob::ALL {
            let (min, max, step) = knob.range();
            let value = knob.shown(&desk.prefs);
            let slider = cx.new(|_| {
                SliderState::new()
                    .min(min)
                    .max(max)
                    .step(step)
                    .default_value(value)
            });
            // Dragging applies live; the release saves once.
            subs.push(cx.subscribe_in(
                &slider,
                window,
                move |desk, _, event: &SliderEvent, _, cx| match event {
                    SliderEvent::Change(value) => {
                        let value = value.start();
                        desk.apply_prefs(|prefs| knob.store(prefs, value), cx);
                    }
                    SliderEvent::Release(value) => {
                        let value = value.start();
                        desk.apply_prefs(|prefs| knob.store(prefs, value), cx);
                        desk.persist();
                    }
                },
            ));
            sliders.push((knob, slider));
        }
        let selected = fonts
            .iter()
            .position(|family| family.as_ref() == desk.prefs.font_family)
            .map(|row| IndexPath::default().row(row));
        let font = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(fonts.iter().cloned().map(FontChoice).collect::<Vec<_>>()),
                selected,
                window,
                cx,
            )
            .searchable(true)
        });
        subs.push(cx.subscribe_in(
            &font,
            window,
            |desk, _, event: &SelectEvent<SearchableVec<FontChoice>>, _, cx| {
                let SelectEvent::Confirm(Some(family)) = event else {
                    return;
                };
                let family = family.to_string();
                desk.update_prefs(|prefs| prefs.font_family = family, cx);
            },
        ));
        desk.settings_controls = Some(SettingsControls {
            sliders,
            font,
            fonts,
            _subs: subs,
        });
    }
    sync_font(desk, window, cx);
    sync_sliders(desk, window, cx);
}

/// Imports and resets update the visible selection without rebuilding the controls.
fn sync_font(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) {
    let Some(controls) = desk.settings_controls.as_mut() else {
        return;
    };
    if controls
        .font
        .read(cx)
        .selected_value()
        .is_some_and(|family| family.as_ref() == desk.prefs.font_family)
    {
        return;
    }
    let current: SharedString = desk.prefs.font_family.clone().into();
    if !controls.fonts.contains(&current) {
        controls.fonts.push(current.clone());
        controls.font.update(cx, |state, cx| {
            state.set_items(
                SearchableVec::new(
                    controls
                        .fonts
                        .iter()
                        .cloned()
                        .map(FontChoice)
                        .collect::<Vec<_>>(),
                ),
                window,
                cx,
            )
        });
    }
    controls.font.update(cx, |state, cx| {
        state.set_selected_value(&current, window, cx)
    });
}

/// A slider follows a preference changed elsewhere (a chord, a reset, a Ghostty import).
fn sync_sliders(desk: &Desk, window: &mut Window, cx: &mut Context<Desk>) {
    let Some(controls) = desk.settings_controls.as_ref() else {
        return;
    };
    for (knob, slider) in &controls.sliders {
        let wanted = knob.shown(&desk.prefs);
        let (_, _, step) = knob.range();
        if (slider.read(cx).value().start() - wanted).abs() > step / 2. {
            slider.update(cx, |slider, cx| slider.set_value(wanted, window, cx));
        }
    }
}

pub(crate) fn panel(
    desk: &mut Desk,
    colors: &Palette,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> AnyElement {
    sync_font(desk, window, cx);
    sync_sliders(desk, window, cx);
    let handle = cx.weak_entity();
    let viewport = window.viewport_size();
    let height = (viewport.height - px(112.)).max(px(240.));
    let width = (viewport.width - px(48.)).min(px(880.)).max(px(280.));
    let light = colors.theme.appearance == "light";
    let ink = from_hex(&colors.theme.foreground);
    let settings = Settings::new("phux-settings")
        .sidebar_width(px(190.))
        .with_group_variant(GroupBoxVariant::Outline)
        .page(appearance_page(desk, &handle, cx))
        .page(window_page(desk, &handle))
        .page(keyboard_page(desk, &handle))
        .page(advanced_page(desk, &handle))
        .page(ghostty_page(desk, &handle))
        .page(super::updates::page(&desk.updates, colors, cx))
        .page(connection_page(desk, &handle));
    div()
        .mt(px(56.))
        .w(width)
        .h(height)
        .rounded(px(14.))
        .bg(from_hex(&colors.raised).opacity(0.97))
        .border_1()
        .border_color(ink.opacity(if light { 0.12 } else { 0.14 }))
        .shadow(vec![
            gpui::BoxShadow {
                color: gpui::black().opacity(if light { 0.14 } else { 0.42 }),
                offset: gpui::point(px(0.), px(20.)),
                blur_radius: px(64.),
                spread_radius: px(-8.),
                inset: false,
            },
            gpui::BoxShadow {
                color: gpui::black().opacity(if light { 0.06 } else { 0.18 }),
                offset: gpui::point(px(0.), px(2.)),
                blur_radius: px(8.),
                spread_radius: px(0.),
                inset: false,
            },
        ])
        .overflow_hidden()
        .occlude()
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .px(px(20.))
                .h(px(64.))
                .flex_shrink_0()
                .bg(ink.opacity(0.025))
                .border_b_1()
                .border_color(ink.opacity(0.08))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(ink)
                                .child("Settings"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(from_hex(&colors.muted))
                                .child("Appearance, layout & keyboard"),
                        ),
                )
                .child(
                    Button::new("settings-close")
                        .icon(IconName::Close)
                        .ghost()
                        .small()
                        .tooltip("Close (esc)")
                        .on_click(cx.listener(|desk, _, window, cx| desk.close_modal(window, cx))),
                ),
        )
        .child(div().flex_1().min_h_0().child(settings))
        .into_any_element()
}

fn prefs_of(handle: &WeakEntity<Desk>, cx: &App) -> DisplayPrefs {
    handle
        .upgrade()
        .map(|desk| desk.read(cx).prefs.clone())
        .unwrap_or_default()
}

fn set_prefs(handle: &WeakEntity<Desk>, cx: &mut App, change: impl FnOnce(&mut DisplayPrefs)) {
    let _ = handle.update(cx, |desk, cx| desk.update_prefs(change, cx));
}

fn switch(
    handle: &WeakEntity<Desk>,
    get: fn(&DisplayPrefs) -> bool,
    set: fn(&mut DisplayPrefs, bool),
) -> SettingField<bool> {
    let reader = handle.clone();
    let writer = handle.clone();
    SettingField::switch(
        move |cx| get(&prefs_of(&reader, cx)),
        move |value, cx| set_prefs(&writer, cx, |prefs| set(prefs, value)),
    )
    .default_value(get(&settings::default_display()))
}

fn number(
    handle: &WeakEntity<Desk>,
    range: (f64, f64, f64),
    get: fn(&DisplayPrefs) -> f64,
    set: fn(&mut DisplayPrefs, f64),
) -> SettingField<f64> {
    let reader = handle.clone();
    let writer = handle.clone();
    let (min, max, step) = range;
    SettingField::number_input(
        NumberFieldOptions { min, max, step },
        move |cx| get(&prefs_of(&reader, cx)),
        move |value, cx| set_prefs(&writer, cx, |prefs| set(prefs, value)),
    )
    .default_value(get(&settings::default_display()))
}

/// A slider with its value beside it, and a reset to the shipped default.
fn slider_item(
    desk: &Desk,
    handle: &WeakEntity<Desk>,
    knob: Knob,
    title: &str,
    description: &str,
) -> Option<SettingItem> {
    let slider = desk
        .settings_controls
        .as_ref()?
        .sliders
        .iter()
        .find(|(owned, _)| *owned == knob)?
        .1
        .clone();
    let reader = handle.clone();
    let dirty = handle.clone();
    let reset = handle.clone();
    let field = SettingField::render(move |_, _, cx| {
        let value = knob.shown(&prefs_of(&reader, cx));
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(Slider::new(&slider).w(px(180.)))
            .child(
                div()
                    .w(px(44.))
                    .text_right()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(knob.readout(value)),
            )
    })
    .on_reset(
        move |cx| knob.shown(&prefs_of(&dirty, cx)) != knob.shown(&settings::default_display()),
        move |_, cx| {
            set_prefs(&reset, cx, |prefs| {
                knob.store(prefs, knob.shown(&settings::default_display()))
            })
        },
    );
    Some(SettingItem::new(title.to_owned(), field).description(description.to_owned()))
}

fn appearance_page(desk: &Desk, handle: &WeakEntity<Desk>, cx: &App) -> SettingPage {
    let themes = {
        let mut themes = theme::themes();
        if let Some(extra) = settings::ghostty_theme(desk.ghostty()) {
            themes.insert(0, extra);
        }
        themes
    };
    let cards_handle = handle.clone();
    let theme_item = SettingItem::render(move |_, _, cx| {
        let selected = prefs_of(&cards_handle, cx).theme_id;
        div()
            .flex()
            .flex_wrap()
            .gap(px(10.))
            .children(
                themes
                    .iter()
                    .map(|theme| theme_card(theme, theme.id == selected, &cards_handle)),
            )
            .into_any_element()
    })
    .keywords(["theme", "colors", "dark", "light"]);
    let mut font_group = SettingGroup::new().title("Font");
    if let Some(controls) = desk.settings_controls.as_ref() {
        let font = controls.font.clone();
        let dirty = handle.clone();
        let reset = handle.clone();
        font_group = font_group.item(
            SettingItem::new(
                "Font family",
                SettingField::render(move |_, _, _| {
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
                })
                .on_reset(
                    move |cx| {
                        prefs_of(&dirty, cx).font_family != settings::default_display().font_family
                    },
                    move |_, cx| {
                        set_prefs(&reset, cx, |prefs| {
                            prefs.font_family = settings::default_display().font_family
                        })
                    },
                ),
            )
            .description({
                let mut font = gpui::font(desk.prefs.font_family.clone());
                crate::fonts::resolve(&mut font, cx);
                if font.family.as_ref() != desk.prefs.font_family {
                    format!(
                        "{} is unavailable; using Paper Mono. Choose an installed font.",
                        desk.prefs.font_family
                    )
                } else {
                    "Installed fonts and the current selection. Search to filter.".to_owned()
                }
            })
            .keywords(["font", "typeface", "monospace", "nerd", "glyphs"]),
        );
    }
    font_group = font_group.item(
        SettingItem::new(
            "Font size",
            number(
                handle,
                (9., 28., 1.),
                |prefs| prefs.font_size,
                |prefs, value| prefs.font_size = value,
            ),
        )
        .description("Points. ⌘= and ⌘- step it from anywhere."),
    );
    let preview_handle = handle.clone();
    let preview = SettingItem::render(move |_, _, cx| {
        preview(&prefs_of(&preview_handle, cx), &preview_handle, cx)
    })
    .keywords(["preview", "sample"]);
    SettingPage::new("Appearance")
        .icon(IconName::Palette)
        .group(font_group)
        .group(SettingGroup::new().title("Preview").item(preview))
        .group(
            SettingGroup::new()
                .title("Theme")
                .description("Ghostty's theme appears first when your config names one.")
                .item(theme_item),
        )
}

fn theme_card(theme: &theme::Theme, selected: bool, handle: &WeakEntity<Desk>) -> AnyElement {
    let palette = theme::palette(theme);
    let id = theme.id.clone();
    let handle = handle.clone();
    let swatch = |color: &str| div().size(px(10.)).rounded_full().bg(from_hex(color));
    div()
        .id(SharedString::from(format!("theme-card-{}", theme.id)))
        .w(px(132.))
        .p(px(10.))
        .rounded(px(10.))
        .bg(from_hex(&theme.background))
        .border_2()
        .border_color(from_hex(if selected {
            &theme.accent
        } else {
            &palette.border
        }))
        .cursor_pointer()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(from_hex(&theme.foreground))
                .child(theme.name.clone()),
        )
        .child(
            div()
                .flex()
                .gap(px(5.))
                .child(swatch(&theme.accent))
                .child(swatch(&theme.success))
                .child(swatch(&theme.warning))
                .child(swatch(&theme.danger)),
        )
        .child(
            div()
                .h(px(4.))
                .w(relative(0.8))
                .rounded_full()
                .bg(from_hex(&theme.foreground)),
        )
        .child(
            div()
                .h(px(4.))
                .w(relative(0.5))
                .rounded_full()
                .bg(from_hex(&palette.muted)),
        )
        .on_click(move |_, _, cx| {
            let id = id.clone();
            set_prefs(&handle, cx, |prefs| prefs.theme_id = id);
        })
        .into_any_element()
}

/// A live theme and font sample; grid spacing is applied to the terminal itself.
fn preview(prefs: &DisplayPrefs, handle: &WeakEntity<Desk>, cx: &App) -> AnyElement {
    let Some(desk) = handle.upgrade() else {
        return div().into_any_element();
    };
    let colors = desk.read(cx).palette();
    let size = prefs.font_size as f32;
    let mut font = gpui::font(prefs.font_family.clone());
    crate::fonts::resolve(&mut font, cx);
    let line = |text: &str, color: &str| div().text_color(from_hex(color)).child(text.to_owned());
    div()
        .w_full()
        .p(px(prefs.padding_y.max(6.) as f32))
        .px(px(prefs.padding_x.max(8.) as f32))
        .rounded(px(8.))
        .bg(from_hex(&colors.theme.background))
        .font_family(font.family)
        .text_size(px(size))
        .line_height(relative(prefs.line_height as f32))
        .child(line("~/src/phux  main  ✓", &colors.theme.accent))
        .child(line("$ cargo test --workspace", &colors.theme.foreground))
        .child(line("   Compiling phux-desktop v0.1.0", &colors.muted))
        .child(line(
            "Aa Bb 0123456789  [] {} ()  -> => !=",
            &colors.theme.foreground,
        ))
        .child(line(
            "test result: ok. 128 passed; 0 failed",
            &colors.theme.success,
        ))
        .into_any_element()
}

fn window_page(desk: &Desk, handle: &WeakEntity<Desk>) -> SettingPage {
    let sidebar_chord = desk
        .shown_chord("sidebar")
        .map(|chord| format!("Toggle it with {chord}."))
        .unwrap_or_default();
    let tabs = handle.clone();
    let tabs_dirty = handle.clone();
    let tabs_reset = handle.clone();
    let tab_position = SettingField::render(move |_, _, cx| {
        let position = prefs_of(&tabs, cx).tab_position;
        let ink = cx.theme().foreground;
        div()
            .tab_group()
            .flex()
            .gap(px(3.))
            .p(px(3.))
            .rounded(px(10.))
            .bg(ink.opacity(0.05))
            .children(
                [
                    (settings::TabPosition::Top, "Top", IconName::LayoutDashboard),
                    (settings::TabPosition::Side, "Side", IconName::PanelLeft),
                ]
                .into_iter()
                .map(|(value, label, icon)| {
                    let writer = tabs.clone();
                    let selected = position == value;
                    Button::new(SharedString::from(format!(
                        "tab-position-{}",
                        value.as_str()
                    )))
                    .label(label)
                    .icon(icon)
                    .small()
                    .ghost()
                    .toggled(selected)
                    .rounded(px(7.))
                    .bg(ink.opacity(if selected { 0.10 } else { 0. }))
                    .text_color(ink.opacity(if selected { 1. } else { 0.65 }))
                    .on_click(move |_, _, cx| {
                        set_prefs(&writer, cx, |prefs| prefs.tab_position = value);
                    })
                }),
            )
    })
    .on_reset(
        move |cx| prefs_of(&tabs_dirty, cx).tab_position != settings::TabPosition::Top,
        move |_, cx| {
            set_prefs(&tabs_reset, cx, |prefs| {
                prefs.tab_position = settings::TabPosition::Top
            });
        },
    );
    let layout = SettingGroup::new()
        .title("Layout")
        .item(
            SettingItem::new("Tab placement", tab_position)
                .description("Top keeps tabs in the titlebar. Side gives them a scrollable rail, independent of the sidebar. Quick windows stay compact."),
        )
        .item(
            SettingItem::new(
                "Show sidebar",
                switch(
                    handle,
                    |prefs| prefs.sidebar_visible,
                    |prefs, value| prefs.sidebar_visible = value,
                ),
            )
            .description(sidebar_chord),
        )
        .items(slider_item(
            desk,
            handle,
            Knob::Glass,
            "Background opacity",
            "Terminal background over the blurred desktop.",
        ));
    let notifications = SettingGroup::new().title("Notifications").item(
        SettingItem::new("Agent notifications", switch(handle, |prefs| prefs.notifications, |prefs, value| prefs.notifications = value))
            .description("Notify when an agent finishes, blocks, or needs attention in another pane. Also shown in Notification Center while Phux is in the background."),
    );
    SettingPage::new("Window")
        .icon(IconName::PanelLeft)
        .group(layout)
        .group(notifications)
}

fn advanced_page(desk: &Desk, handle: &WeakEntity<Desk>) -> SettingPage {
    let spacing = SettingGroup::new()
        .title("Terminal spacing")
        .description("Fine-tune the terminal grid. Defaults work with Paper Mono.")
        .items(slider_item(
            desk,
            handle,
            Knob::LineHeight,
            "Line height",
            "Row height as a multiple of the font size.",
        ))
        .items(slider_item(
            desk,
            handle,
            Knob::CellWidth,
            "Cell width",
            "Column width as a percent of the font's advance.",
        ))
        .item(
            SettingItem::new(
                "Horizontal padding",
                number(
                    handle,
                    (0., 64., 1.),
                    |prefs| prefs.padding_x,
                    |prefs, value| prefs.padding_x = value,
                ),
            )
            .description("Points between the pane edge and the text."),
        )
        .item(
            SettingItem::new(
                "Vertical padding",
                number(
                    handle,
                    (0., 64., 1.),
                    |prefs| prefs.padding_y,
                    |prefs, value| prefs.padding_y = value,
                ),
            )
            .description("Points above and below the text."),
        );
    SettingPage::new("Advanced")
        .icon(IconName::Frame)
        .group(spacing)
        .group(SettingGroup::new().title("Pane dimming").items(slider_item(
            desk,
            handle,
            Knob::Unfocused,
            "Unfocused panes",
            "Visibility of panes you are not typing in. 100% disables dimming.",
        )))
}

fn keyboard_page(desk: &Desk, handle: &WeakEntity<Desk>) -> SettingPage {
    let input = SettingGroup::new()
        .title("Input")
        .item(
            SettingItem::new("Option sends Alt", switch(handle, |prefs| prefs.option_as_alt, |prefs, value| prefs.option_as_alt = value))
                .description("Off keeps macOS Option characters such as å and ß."),
        )
        .item(
            SettingItem::new("Use Ghostty keybinds", switch(handle, |prefs| prefs.ghostty_keys, |prefs, value| prefs.ghostty_keys = value))
                .description("Your Ghostty keybind lines override these shortcuts, and global: binds toggle the quick terminal."),
        );
    let catalog = settings::builtin_commands();
    let shown = settings::shown_chords(desk.keymap(), &settings::default_chords());
    let mut rows: Vec<(String, String, String)> = catalog
        .iter()
        .filter(|command| !command.is_tab_jump())
        .filter_map(|command| {
            shown
                .iter()
                .find(|(id, _)| id == command.id)
                .map(|(_, chord)| {
                    (
                        command.title.to_owned(),
                        command.group.to_owned(),
                        chord.clone(),
                    )
                })
        })
        .collect();
    // Ghostty binds with no catalog entry (send:, font-size:, …) are listed by their action.
    for (chord, id) in desk.keymap() {
        if !catalog.iter().any(|command| command.id == id) {
            rows.push((id.clone(), "Ghostty".to_owned(), chord.clone()));
        }
    }
    let shortcuts = SettingGroup::new()
        .title("Shortcuts")
        .description("Search above to find a command or a chord.")
        .items(rows.into_iter().map(|(title, group, chord)| {
            let shown = settings::display_chord(&chord);
            let keystroke = gpui_stroke(&chord).and_then(|stroke| Keystroke::parse(&stroke).ok());
            SettingItem::new(
                title,
                SettingField::render(move |_, _, cx| match keystroke.clone() {
                    Some(keystroke) => Kbd::new(keystroke).into_any_element(),
                    None => div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(shown.clone())
                        .into_any_element(),
                }),
            )
            .description(group.clone())
            .keywords([group, settings::display_chord(&chord)])
        }));
    SettingPage::new("Keyboard")
        .icon(IconName::ALargeSmall)
        .group(input)
        .group(shortcuts)
}

fn ghostty_page(desk: &Desk, handle: &WeakEntity<Desk>) -> SettingPage {
    let found = desk.ghostty_found;
    let font = desk.ghostty().font_family.clone();
    let ignored = desk.ghostty().unmapped.len();
    let apply = handle.clone();
    let reload = handle.clone();
    let status = SettingItem::render(move |_, _, cx| {
        let muted = cx.theme().muted_foreground;
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .text_sm()
            .child(if found {
                "Ghostty config found."
            } else {
                "No Ghostty config found."
            })
            .children(font.clone().map(|font| {
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("Font: {font}"))
            }))
            .when(ignored > 0, |status| {
                status.child(div().text_xs().text_color(muted).child(format!(
                    "{ignored} {} no phux equivalent and {} ignored.",
                    if ignored == 1 {
                        "line has"
                    } else {
                        "lines have"
                    },
                    if ignored == 1 { "is" } else { "are" },
                )))
            })
            .into_any_element()
    });
    let actions = SettingItem::render(move |_, _, _| {
        let apply = apply.clone();
        let reload = reload.clone();
        div()
            .flex()
            .gap(px(8.))
            .child(
                Button::new("ghostty-apply")
                    .label("Use Ghostty look")
                    .primary()
                    .small()
                    .disabled(!found)
                    .on_click(move |_, _, cx| {
                        let _ = apply.update(cx, |desk, cx| desk.import_ghostty(cx));
                    }),
            )
            .child(
                Button::new("ghostty-reload")
                    .label("Reload config")
                    .small()
                    .on_click(move |_, _, cx| {
                        let _ = reload.update(cx, |desk, cx| desk.reload_ghostty(cx));
                    }),
            )
            .into_any_element()
    });
    SettingPage::new("Ghostty").icon(IconName::Frame).group(
        SettingGroup::new()
            .title("Config")
            .description("Use Ghostty look copies its theme, font, size, cell size and padding into these settings.")
            .item(status)
            .item(actions),
    )
}

fn connection_page(desk: &Desk, handle: &WeakEntity<Desk>) -> SettingPage {
    let (status, _) = desk.connection_label();
    let fact = |title: &str, value: String| {
        SettingItem::new(
            title.to_owned(),
            SettingField::render(move |_, _, _| div().text_sm().child(value.clone())),
        )
    };
    let reconnect = handle.clone();
    let attached = desk
        .link
        .as_ref()
        .is_some_and(|link| link.connection.status == Status::Attached);
    let action = SettingItem::new(
        "Reconnect",
        SettingField::render(move |_, _, _| {
            let reconnect = reconnect.clone();
            Button::new("conn-reconnect")
                .label("Reconnect")
                .small()
                .map(|button| if attached { button } else { button.primary() })
                .on_click(move |_, _, cx| {
                    let _ = reconnect.update(cx, |desk, cx| desk.reconnect(cx));
                })
        }),
    )
    .description("Reconnect and restore this window.");
    SettingPage::new("Connection")
        .icon(IconName::Network)
        .group(
            SettingGroup::new()
                .item(fact("Status", status))
                .item(action),
        )
}
