//! Settings on Kit [`Settings`]: searchable pages of switches, sliders, number
//! fields and a font select. Every control writes its preference directly, so
//! Settings stays open, and every control moves both ways with a reset.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, Keystroke, SharedString, Subscription, WeakEntity, Window,
    div, px, relative,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
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

type FontSelect = SelectState<SearchableVec<SharedString>>;

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

/// The family list: the Ghostty face, the current face, then the built-ins, without repeats.
fn font_families(desk: &Desk) -> Vec<SharedString> {
    let mut families: Vec<SharedString> = Vec::new();
    let candidates = desk
        .ghostty()
        .font_family
        .iter()
        .cloned()
        .chain(std::iter::once(desk.prefs.font_family.clone()))
        .chain(
            settings::FONT_FAMILIES
                .iter()
                .map(|family| (*family).to_owned()),
        );
    for family in candidates {
        let family = family.trim().to_owned();
        if !family.is_empty() && !families.iter().any(|known| known.as_ref() == family) {
            families.push(family.into());
        }
    }
    families
}

/// Build the controls once, then bring every value in line with the current prefs.
pub(crate) fn prepare(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) {
    if desk.settings_controls.is_none() {
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
        let fonts = font_families(desk);
        let selected = fonts
            .iter()
            .position(|family| family.as_ref() == desk.prefs.font_family)
            .map(|row| IndexPath::default().row(row));
        let font = cx.new(|cx| {
            SelectState::new(SearchableVec::new(fonts.clone()), selected, window, cx)
                .searchable(true)
        });
        subs.push(cx.subscribe_in(
            &font,
            window,
            |desk, _, event: &SelectEvent<SearchableVec<SharedString>>, _, cx| {
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
    let fonts = font_families(desk);
    let current: SharedString = desk.prefs.font_family.clone().into();
    let Some(controls) = desk.settings_controls.as_mut() else {
        return;
    };
    if controls.fonts != fonts {
        controls.fonts = fonts.clone();
        controls.font.update(cx, |state, cx| {
            state.set_items(SearchableVec::new(fonts), window, cx)
        });
    }
    controls.font.update(cx, |state, cx| {
        state.set_selected_value(&current, window, cx)
    });
    sync_sliders(desk, window, cx);
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
    sync_sliders(desk, window, cx);
    let handle = cx.weak_entity();
    let height = (window.viewport_size().height - px(112.)).max(px(320.));
    let settings = Settings::new("phux-settings")
        .sidebar_width(px(190.))
        .page(appearance_page(desk, &handle))
        .page(window_page(desk, &handle))
        .page(keyboard_page(desk, &handle))
        .page(ghostty_page(desk, &handle))
        .page(connection_page(desk, &handle));
    div()
        .mt(px(56.))
        .w(px(820.))
        .h(height)
        .rounded(px(12.))
        .bg(from_hex(&colors.theme.surface))
        .border_1()
        .border_color(from_hex(&colors.border))
        .shadow_lg()
        .overflow_hidden()
        .occlude()
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .px(px(16.))
                .h(px(44.))
                .border_b_1()
                .border_color(from_hex(&colors.border))
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(from_hex(&colors.theme.foreground))
                        .child("Settings"),
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

fn appearance_page(desk: &Desk, handle: &WeakEntity<Desk>) -> SettingPage {
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
        font_group = font_group.item(
            SettingItem::new(
                "Family",
                SettingField::render(move |_, _, _| {
                    Select::new(&font).small().w(px(220.)).menu_max_h(px(280.))
                }),
            )
            .description("Your Ghostty font comes first. Search to filter."),
        );
    }
    font_group = font_group.item(
        SettingItem::new(
            "Size",
            number(
                handle,
                (9., 28., 1.),
                |prefs| prefs.font_size,
                |prefs, value| prefs.font_size = value,
            ),
        )
        .description("Points. ⌘= and ⌘- step it from anywhere."),
    );
    font_group = font_group.items(slider_item(
        desk,
        handle,
        Knob::LineHeight,
        "Line height",
        "Space between rows, as a multiple of the font size.",
    ));
    font_group = font_group.items(slider_item(
        desk,
        handle,
        Knob::CellWidth,
        "Cell width",
        "Column width, as a percent of the font's advance.",
    ));
    let preview_handle = handle.clone();
    let preview = SettingItem::render(move |_, _, cx| {
        preview(&prefs_of(&preview_handle, cx), &preview_handle, cx)
    })
    .keywords(["preview", "sample"]);
    SettingPage::new("Appearance")
        .icon(IconName::Palette)
        .group(
            SettingGroup::new()
                .title("Theme")
                .description("Ghostty's theme appears first when your config names one.")
                .item(theme_item),
        )
        .group(font_group)
        .group(SettingGroup::new().title("Preview").item(preview))
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

/// The chosen theme, face, size, line height and cell width, painted like the terminal.
fn preview(prefs: &DisplayPrefs, handle: &WeakEntity<Desk>, cx: &App) -> AnyElement {
    let Some(desk) = handle.upgrade() else {
        return div().into_any_element();
    };
    let colors = desk.read(cx).palette();
    let size = prefs.font_size as f32;
    let line = |text: &str, color: &str| div().text_color(from_hex(color)).child(text.to_owned());
    div()
        .w_full()
        .p(px(prefs.padding_y.max(6.) as f32))
        .px(px(prefs.padding_x.max(8.) as f32))
        .rounded(px(8.))
        .bg(from_hex(&colors.theme.background))
        .font_family(prefs.font_family.clone())
        .text_size(px(size))
        .line_height(relative(prefs.line_height as f32))
        .child(line("~/src/phux  main  ✓", &colors.theme.accent))
        .child(line("$ cargo test --workspace", &colors.theme.foreground))
        .child(line("   Compiling phux-desktop v0.1.0", &colors.muted))
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
    let layout = SettingGroup::new()
        .title("Layout")
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
        )
        .items(slider_item(
            desk,
            handle,
            Knob::Unfocused,
            "Unfocused panes",
            "How much panes you are not typing in stay visible. 100% is no dimming.",
        ))
        .items(slider_item(
            desk,
            handle,
            Knob::Glass,
            "Glass",
            "Terminal background opacity over the blurred desktop.",
        ));
    let notifications = SettingGroup::new().title("Notifications").item(
        SettingItem::new("Agent notifications", switch(handle, |prefs| prefs.notifications, |prefs, value| prefs.notifications = value))
            .description("Toast when an agent finishes, blocks, or needs attention in a pane you are not looking at."),
    );
    SettingPage::new("Window")
        .icon(IconName::PanelLeft)
        .group(layout)
        .group(notifications)
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
    let apply = handle.clone();
    let reload = handle.clone();
    let status = SettingItem::render(move |_, _, cx| {
        let muted = cx.theme().muted_foreground;
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .text_sm()
            .child(if found { "Ghostty config found." } else { "No Ghostty config found." })
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("Phux reads $XDG_CONFIG_HOME/ghostty/config (or config.ghostty), then ~/Library/Application Support/com.mitchellh.ghostty/config, with config-file includes and theme =."),
            )
            .children(font.clone().map(|font| div().text_xs().text_color(muted).child(format!("Font: {font}"))))
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
    let mut page = SettingPage::new("Ghostty").icon(IconName::Frame).group(
        SettingGroup::new()
            .title("Config")
            .description("Use Ghostty look copies its theme, font, size, cell size and padding into these settings.")
            .item(status)
            .item(actions),
    );
    let unmapped = desk.ghostty().unmapped.clone();
    if !unmapped.is_empty() {
        let mono = cx_mono();
        page = page.group(
            SettingGroup::new()
                .title("Not supported here")
                .description("These lines are read but have no phux equivalent yet.")
                .item(SettingItem::render(move |_, _, cx| {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .font_family(mono.clone())
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .children(unmapped.iter().map(|line| div().child(line.clone())))
                        .into_any_element()
                })),
        );
    }
    page
}

fn cx_mono() -> SharedString {
    "Menlo".into()
}

fn connection_page(desk: &Desk, handle: &WeakEntity<Desk>) -> SettingPage {
    let link = desk.link.as_ref();
    let (status, _) = desk.connection_label();
    let server = link
        .and_then(|link| link.connection.server_id())
        .unwrap_or_else(|| "—".into());
    let protocol = link
        .and_then(|link| link.connection.server.as_ref())
        .map(phux_client_ffi::projection::status::protocol_version)
        .unwrap_or_else(|| "—".into());
    let epoch = link
        .map(|link| link.connection.connection_epoch().to_string())
        .unwrap_or_else(|| "—".into());
    let features = link
        .map(|link| link.connection.features().join(", "))
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "—".into());
    let fact = |title: &str, value: String| {
        SettingItem::new(
            title.to_owned(),
            SettingField::render(move |_, _, _| {
                div()
                    .max_w(px(360.))
                    .text_sm()
                    .font_family(cx_mono())
                    .truncate()
                    .child(value.clone())
            }),
        )
    };
    let reconnect = handle.clone();
    let attached = link.is_some_and(|link| link.connection.status == Status::Attached);
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
    .description("Release every view, re-dial the socket, and restore this window's layout.");
    SettingPage::new("Connection")
        .icon(IconName::Network)
        .group(
            SettingGroup::new()
                .title("Server")
                .item(fact("Status", status))
                .item(fact("Socket", desk.socket.display().to_string()))
                .item(fact("Home session", desk.session.clone()))
                .item(fact("Server id", server))
                .item(fact("Protocol", protocol))
                .item(fact("Connection epoch", epoch))
                .item(fact("Capabilities", features))
                .item(action),
        )
}
