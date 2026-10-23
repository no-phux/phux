//! Settings on Kit [`Settings`]: searchable pages of switches, sliders, number
//! fields and a font select. Every control writes its preference directly, so
//! Settings stays open, and every control moves both ways with a reset.

mod appearance;
pub(super) mod catalog;
mod connections;
mod developer;
mod input;
mod items;
mod layout;
mod palette;
#[cfg(feature = "ui-fixtures")]
pub(super) mod preview;
pub(crate) mod window;
#[cfg(test)]
pub(super) mod window_tests;

pub(crate) use palette::{editor, entries};

use items::*;

#[cfg(test)]
pub(super) use connections::machine_action;

use catalog::Item as SettingItem;
use gpui::prelude::*;
use gpui::{
    App, ClickEvent, Context, Entity, SharedString, Subscription, WeakEntity, Window, div, px,
};
use gpui_kit::component::group_box::GroupBoxVariant;
use gpui_kit::component::select::{SearchableVec, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::setting::{NumberFieldOptions, SettingField, Settings};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{ActiveTheme, IndexPath};

use crate::settings::theme;
use crate::settings::{self, DisplayPrefs};

use super::desk::Desk;
use super::skin::Look;

type FontSelect = SelectState<SearchableVec<FontChoice>>;

/// Search destinations; detail pages reuse the existing preference controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Page {
    Appearance,
    Layout,
    Keyboard,
    Spacing,
    Ghostty,
    Updates,
    Connections,
    Developer,
}

impl Page {
    pub const ALL: [Self; 8] = [
        Self::Appearance,
        Self::Layout,
        Self::Keyboard,
        Self::Spacing,
        Self::Ghostty,
        Self::Updates,
        Self::Connections,
        Self::Developer,
    ];
    pub fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Layout => "Layout & notifications",
            Self::Keyboard => "Keyboard & shortcuts",
            Self::Spacing => "Terminal spacing",
            Self::Ghostty => "Ghostty integration",
            Self::Updates => "Updates",
            Self::Connections => "Hosts & connections",
            Self::Developer => "Developer",
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::Appearance => "Theme, font, skin and plain terminal",
            Self::Layout => "Tabs, sidebar, blur, glass and agent alerts",
            Self::Keyboard => "Input behavior and every shortcut",
            Self::Spacing => "Line height, cell width, padding and dimming",
            Self::Ghostty => "Import or reload your Ghostty preferences",
            Self::Updates => "Version, downloads and updates",
            Self::Connections => "Your machines, connection status and recovery",
            Self::Developer => "Developer mode, profiling, traces, logs and diagnostics",
        }
    }

    pub fn keywords(self) -> &'static str {
        match self {
            Self::Appearance => {
                "theme colors dark light font family size typeface skin look glass plain terminal \
                 animations motion laser cursor native agents conversation chat acp transcript"
            }
            Self::Layout => {
                "sidebar tabs top bottom side background opacity glass blur solid opaque notifications alerts experimental agent fleet dashboard overview"
            }
            Self::Keyboard => {
                "option alt keys keybind shortcuts chord input conflicts duplicate source"
            }
            Self::Spacing => "line height cell width padding horizontal vertical unfocused dimming",
            Self::Ghostty => {
                "import reload config ghostty look integration skipped malformed unsupported lines"
            }
            Self::Updates => "version release install update download channel",
            Self::Connections => {
                "hosts machines remote ssh local add remove retry reconnect status endpoint"
            }
            Self::Developer => {
                "developer dev mode debug debugging telemetry socket performance hud heap rust allocation allocations counting counters profiling trace perfetto log logs diagnostics"
            }
        }
    }
}

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
        div()
            .w_full()
            .py(px(5.))
            .font_family(font.family)
            .text_size(px(13.))
            .truncate()
            .child(self.0.clone())
    }
}

/// Slider and select state for one window's Settings. Built on first open.
pub(crate) struct SettingsControls {
    sliders: Vec<(Knob, Entity<SliderState>)>,
    font: Entity<FontSelect>,
    fonts: Vec<SharedString>,
    _subs: Vec<Subscription>,
}

/// One preference, as a path into [`DisplayPrefs`].
type Pref<T> = fn(&mut DisplayPrefs) -> &mut T;

fn read<T: Clone>(pref: Pref<T>, mut prefs: DisplayPrefs) -> T {
    pref(&mut prefs).clone()
}

/// A continuous preference, shown as a multiplier or a percent.
#[derive(Clone, Copy)]
struct Knob {
    title: &'static str,
    /// Display range and step.
    range: (f32, f32, f32),
    percent: bool,
    pref: Pref<f64>,
}

impl Knob {
    const LINE_HEIGHT: Self = Self {
        title: "Line height",
        range: (1., 2., 0.05),
        percent: false,
        pref: |prefs| &mut prefs.line_height,
    };
    const CELL_WIDTH: Self = Self {
        title: "Cell width",
        range: (50., 200., 1.),
        percent: true,
        pref: |prefs| &mut prefs.cell_width,
    };
    const UNFOCUSED: Self = Self {
        title: "Unfocused panes",
        range: (15., 100., 5.),
        percent: true,
        pref: |prefs| &mut prefs.unfocused_opacity,
    };
    const GLASS: Self = Self {
        title: "Pane opacity",
        range: (55., 100., 5.),
        percent: true,
        pref: |prefs| &mut prefs.terminal_opacity,
    };

    fn scale(self) -> f64 {
        if self.percent { 100. } else { 1. }
    }

    fn shown(self, prefs: &DisplayPrefs) -> f32 {
        (read(self.pref, prefs.clone()) * self.scale()) as f32
    }

    fn store(self, prefs: &mut DisplayPrefs, value: f32) {
        *(self.pref)(prefs) = f64::from(value) / self.scale();
    }

    fn readout(self, value: f32) -> String {
        if self.percent {
            format!("{value:.0}%")
        } else {
            format!("{value:.2}")
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
        .clone()
        .filter(|family| installed.contains(family));
    let pinned = [
        crate::fonts::BUNDLED.to_owned(),
        crate::fonts::SYMBOLS.to_owned(),
        desk.prefs.font_family.clone(),
    ]
    .into_iter()
    .chain(ghostty)
    .map(|family| family.trim().to_owned());
    let mut families: Vec<SharedString> = Vec::new();
    for family in pinned.chain(installed) {
        if !family.is_empty() && !families.iter().any(|known| known.as_ref() == family) {
            families.push(family.into());
        }
    }
    families
}

fn font_items(fonts: &[SharedString]) -> SearchableVec<FontChoice> {
    SearchableVec::new(fonts.iter().cloned().map(FontChoice).collect::<Vec<_>>())
}

/// Build the controls once, then bring every value in line with the current prefs.
pub(crate) fn prepare(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) {
    if desk.settings_controls.is_none() {
        desk.settings_controls = Some(build_controls(desk, window, cx));
    }
    sync_controls(desk, window, cx);
}

fn build_controls(desk: &Desk, window: &mut Window, cx: &mut Context<Desk>) -> SettingsControls {
    let fonts = font_families(desk, cx);
    let mut subs = Vec::new();
    let mut sliders = Vec::new();
    for knob in [
        Knob::LINE_HEIGHT,
        Knob::CELL_WIDTH,
        Knob::UNFOCUSED,
        Knob::GLASS,
    ] {
        let (min, max, step) = knob.range;
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
            move |desk, _, event: &SliderEvent, _, cx| {
                let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
                let value = value.start();
                desk.apply_prefs(|prefs| knob.store(prefs, value), cx);
                if matches!(event, SliderEvent::Release(_)) {
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
    let font =
        cx.new(|cx| SelectState::new(font_items(&fonts), selected, window, cx).searchable(true));
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
    SettingsControls {
        sliders,
        font,
        fonts,
        _subs: subs,
    }
}

/// Chords, resets and imports move the visible controls without rebuilding them.
fn sync_controls(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) {
    let Some(controls) = desk.settings_controls.as_mut() else {
        return;
    };
    controls.sync(&desk.prefs, window, cx);
}

impl SettingsControls {
    fn sync(&mut self, prefs: &DisplayPrefs, window: &mut Window, cx: &mut App) {
        let controls = self;
        for (knob, slider) in &controls.sliders {
            let wanted = knob.shown(prefs);
            if (slider.read(cx).value().start() - wanted).abs() > knob.range.2 / 2. {
                slider.update(cx, |slider, cx| slider.set_value(wanted, window, cx));
            }
        }
        if controls
            .font
            .read(cx)
            .selected_value()
            .is_some_and(|family| family.as_ref() == prefs.font_family)
        {
            return;
        }
        let current: SharedString = prefs.font_family.clone().into();
        if !controls.fonts.contains(&current) {
            controls.fonts.push(current.clone());
            let items = font_items(&controls.fonts);
            controls
                .font
                .update(cx, |state, cx| state.set_items(items, window, cx));
        }
        controls.font.update(cx, |state, cx| {
            state.set_selected_value(&current, window, cx)
        });
    }
}

fn content(
    desk: &Desk,
    page: Page,
    handle: &WeakEntity<Desk>,
    controls: Option<&SettingsControls>,
    cx: &App,
) -> catalog::Content {
    let look = desk.look();
    match page {
        Page::Appearance => appearance::appearance_page(desk, handle, controls, cx),
        Page::Layout => layout::window_page(desk, handle, controls),
        Page::Keyboard => input::keyboard_page(desk, handle),
        Page::Spacing => layout::advanced_page(desk, handle, controls),
        Page::Ghostty => input::ghostty_page(desk, handle),
        Page::Updates => {
            let moves = desk.prefs.animations && !cx.reduce_motion();
            super::updates::page(&desk.updates, &look, &desk.prefs.font_family, moves, cx)
        }
        Page::Connections => connections::connection_page(desk, handle),
        Page::Developer => developer::page(handle),
    }
}

fn native_settings(id: SharedString, page: gpui_kit::component::setting::SettingPage) -> Settings {
    Settings::new(id)
        .sidebar_width(px(0.))
        .sidebar_size_range(px(0.)..px(0.))
        .sidebar_style(&gpui::StyleRefinement {
            display: Some(gpui::Display::None),
            ..Default::default()
        })
        .with_group_variant(GroupBoxVariant::Normal)
        .page(page)
}
