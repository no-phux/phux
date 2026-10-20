//! Settings on Kit [`Settings`]: searchable pages of switches, sliders, number
//! fields and a font select. Every control writes its preference directly, so
//! Settings stays open, and every control moves both ways with a reset.

mod appearance;
pub(super) mod catalog;
mod connections;
mod input;
mod layout;
mod palette;
mod performance;

pub(crate) use palette::{editor, entries};

#[cfg(test)]
pub(super) use connections::machine_action;

use catalog::Item as SettingItem;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, SharedString, Subscription, WeakEntity, Window,
    div, px,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::group_box::GroupBoxVariant;
use gpui_kit::component::select::{SearchableVec, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::setting::{NumberFieldOptions, SettingField, Settings};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{ActiveTheme, IconName, IndexPath, Sizable};

use crate::settings::theme;
use crate::settings::{self, DisplayPrefs};

use super::desk::{Desk, Modal};
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
    Performance,
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
        Self::Performance,
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
            Self::Performance => "Performance",
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
            Self::Performance => "Live Rust allocation counting and diagnostic controls",
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
            Self::Performance => {
                "performance heap rust allocation allocations counting counters profiling diagnostics"
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
        title: "Background opacity",
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
        let font = cx
            .new(|cx| SelectState::new(font_items(&fonts), selected, window, cx).searchable(true));
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
    sync_controls(desk, window, cx);
}

/// Chords, resets and imports move the visible controls without rebuilding them.
fn sync_controls(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) {
    let Some(controls) = desk.settings_controls.as_mut() else {
        return;
    };
    for (knob, slider) in &controls.sliders {
        let wanted = knob.shown(&desk.prefs);
        if (slider.read(cx).value().start() - wanted).abs() > knob.range.2 / 2. {
            slider.update(cx, |slider, cx| slider.set_value(wanted, window, cx));
        }
    }
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
        let items = font_items(&controls.fonts);
        controls
            .font
            .update(cx, |state, cx| state.set_items(items, window, cx));
    }
    controls.font.update(cx, |state, cx| {
        state.set_selected_value(&current, window, cx)
    });
}

pub(crate) fn panel(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    sync_controls(desk, window, cx);
    let page = desk.settings_page.unwrap_or(Page::Appearance);
    let content = content(desk, page, &cx.weak_entity(), cx);
    let settings = native_settings(
        SharedString::from(format!("phux-settings-{page:?}")),
        content.inner.title(page.title()),
    );
    let viewport = window.viewport_size();
    let gutter = super::overlays::SHEET_GUTTER;
    let height = (viewport.height.as_f32() - 2. * gutter).clamp(0., 600.);
    let look = desk.look();
    super::overlays::card(
        &look,
        (viewport.width.as_f32() - 2. * gutter).clamp(0., 720.),
    )
    .id("settings-panel")
    .debug_selector(|| "settings-panel".into())
    .h(px(height))
    .overflow_hidden()
    .flex()
    .flex_col()
    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
    .child(super::overlays::launcher::hub_header(
        desk,
        &look.chrome,
        cx,
    ))
    .child(
        div().h(px(40.)).px(px(14.)).flex().items_center().child(
            Button::new("settings-back")
                .label("All settings")
                .icon(IconName::ChevronLeft)
                .small()
                .ghost()
                .on_click(
                    cx.listener(|desk, _, window, cx| desk.open_modal(Modal::Settings, window, cx)),
                ),
        ),
    )
    .child(div().flex_1().min_h_0().child(settings))
    .into_any_element()
}

fn content(desk: &Desk, page: Page, handle: &WeakEntity<Desk>, cx: &App) -> catalog::Content {
    let look = desk.look();
    match page {
        Page::Appearance => appearance::appearance_page(desk, handle, cx),
        Page::Layout => layout::window_page(desk, handle),
        Page::Keyboard => input::keyboard_page(desk, handle),
        Page::Spacing => layout::advanced_page(desk, handle),
        Page::Ghostty => input::ghostty_page(desk, handle),
        Page::Updates => super::updates::page(&desk.updates, &look, &desk.prefs.font_family, cx),
        Page::Connections => connections::connection_page(desk, handle),
        Page::Performance => performance::page(handle),
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

fn prefs_of(handle: &WeakEntity<Desk>, cx: &App) -> DisplayPrefs {
    handle
        .upgrade()
        .map(|desk| desk.read(cx).prefs.clone())
        .unwrap_or_default()
}

/// The desk's look this frame, or the default skin's before the desk is up.
fn look_of(handle: &WeakEntity<Desk>, cx: &App) -> std::rc::Rc<Look> {
    handle.upgrade().map_or_else(
        || {
            let prefs = settings::default_display();
            let colors = theme::palette(&theme::theme_by_id(&prefs.theme_id, &[]));
            std::rc::Rc::new(Look::of(
                settings::skin_by_id(&prefs.skin_id),
                &colors,
                &prefs,
            ))
        },
        |desk| desk.read(cx).look(),
    )
}

fn set_prefs(handle: &WeakEntity<Desk>, cx: &mut App, change: impl FnOnce(&mut DisplayPrefs)) {
    let _ = handle.update(cx, |desk, cx| desk.update_prefs(change, cx));
}

type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A click that changes the prefs.
fn click_sets(handle: &WeakEntity<Desk>, change: impl Fn(&mut DisplayPrefs) + 'static) -> OnClick {
    let handle = handle.clone();
    Box::new(move |_, _, cx| set_prefs(&handle, cx, &change))
}

/// A click that runs `action` on the desk while it is open.
fn click_runs(
    handle: &WeakEntity<Desk>,
    action: impl Fn(&mut Desk, &mut Window, &mut Context<Desk>) + 'static,
) -> OnClick {
    let handle = handle.clone();
    Box::new(move |_, window, cx| {
        let _ = handle.update(cx, |desk, cx| action(desk, window, cx));
    })
}

fn toggle(
    handle: &WeakEntity<Desk>,
    title: &'static str,
    pref: Pref<bool>,
    description: impl Into<String>,
) -> SettingItem {
    let (reader, writer) = (handle.clone(), handle.clone());
    SettingItem::switch(
        title,
        move |cx| read(pref, prefs_of(&reader, cx)),
        move |value, cx| set_prefs(&writer, cx, |prefs| *pref(prefs) = value),
        Some(read(pref, settings::default_display())),
    )
    .description(description.into())
}

fn stepper(
    handle: &WeakEntity<Desk>,
    title: &'static str,
    (min, max, step): (f64, f64, f64),
    pref: Pref<f64>,
    description: impl Into<String>,
) -> SettingItem {
    let (reader, writer) = (handle.clone(), handle.clone());
    let field = SettingField::number_input(
        NumberFieldOptions { min, max, step },
        move |cx| read(pref, prefs_of(&reader, cx)),
        move |value, cx| set_prefs(&writer, cx, |prefs| *pref(prefs) = value),
    )
    .default_value(read(pref, settings::default_display()));
    SettingItem::new(title, field).description(description.into())
}

/// A reset to the shipped default for a custom-rendered `pref`.
fn resets<T: Clone + PartialEq + 'static, F>(
    field: SettingField<F>,
    handle: &WeakEntity<Desk>,
    pref: Pref<T>,
) -> SettingField<F> {
    let (dirty, reset) = (handle.clone(), handle.clone());
    let shipped = move || read(pref, settings::default_display());
    field.on_reset(
        move |cx| read(pref, prefs_of(&dirty, cx)) != shipped(),
        move |_, cx| set_prefs(&reset, cx, |prefs| *pref(prefs) = shipped()),
    )
}

/// A line of text beside its title.
fn fact(title: impl Into<SharedString>, value: String) -> SettingItem {
    SettingItem::new(
        title,
        SettingField::render(move |_, _, _| div().text_sm().child(value.clone())),
    )
    .information()
}

/// A slider with its value beside it, and a reset to the shipped default.
fn slider_item(
    desk: &Desk,
    handle: &WeakEntity<Desk>,
    knob: Knob,
    description: impl Into<String>,
) -> Option<SettingItem> {
    let slider = desk
        .settings_controls
        .as_ref()?
        .sliders
        .iter()
        .find(|(owned, _)| owned.title == knob.title)?
        .1
        .clone();
    let (reader, dirty, reset) = (handle.clone(), handle.clone(), handle.clone());
    let shipped = move || knob.shown(&settings::default_display());
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
        move |cx| knob.shown(&prefs_of(&dirty, cx)) != shipped(),
        move |_, cx| set_prefs(&reset, cx, |prefs| knob.store(prefs, shipped())),
    );
    Some(SettingItem::new(knob.title, field).description(description.into()))
}

/// `text`, then where the value came from and when an edit to that source applies.
fn sourced(
    desk: &Desk,
    text: &str,
    ghostty_keys: &[&str],
    field: fn(&DisplayPrefs) -> String,
) -> String {
    let source = settings::display_provenance(&desk.prefs, desk.ghostty(), ghostty_keys, field);
    notes([text, &source.label(), source.applies().label()])
}

/// Non-empty notes joined into one description line.
fn notes<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    parts
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}
