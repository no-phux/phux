//! The builders every page's rows share: a preference's switch, stepper,
//! slider, reset, read-only fact and source note, and the clicks that write it.

use super::*;

pub(super) fn prefs_of(handle: &WeakEntity<Desk>, cx: &App) -> DisplayPrefs {
    handle
        .upgrade()
        .map(|desk| desk.read(cx).prefs.clone())
        .unwrap_or_default()
}

/// The desk's look this frame, or the default skin's before the desk is up.
pub(super) fn look_of(handle: &WeakEntity<Desk>, cx: &App) -> std::rc::Rc<Look> {
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

pub(super) fn set_prefs(
    handle: &WeakEntity<Desk>,
    cx: &mut App,
    change: impl FnOnce(&mut DisplayPrefs),
) {
    let _ = handle.update(cx, |desk, cx| desk.update_prefs(change, cx));
}

pub(super) type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A click that changes the prefs.
pub(super) fn click_sets(
    handle: &WeakEntity<Desk>,
    change: impl Fn(&mut DisplayPrefs) + 'static,
) -> OnClick {
    let handle = handle.clone();
    Box::new(move |_, _, cx| set_prefs(&handle, cx, &change))
}

/// A click that runs `action` on the desk while it is open.
pub(super) fn click_runs(
    handle: &WeakEntity<Desk>,
    action: impl Fn(&mut Desk, &mut Window, &mut Context<Desk>) + 'static,
) -> OnClick {
    let handle = handle.clone();
    let action = std::rc::Rc::new(action);
    Box::new(move |_, window, cx| {
        let Some(desk) = handle.upgrade() else {
            return;
        };
        let owner = desk
            .read(cx)
            .desktop
            .read(cx)
            .desk_windows()
            .find(|(_, candidate, _)| candidate.entity_id() == desk.entity_id())
            .map(|(window, _, _)| window);
        if let Some(owner) = owner
            && owner.window_id() != window.window_handle().window_id()
        {
            // Dialogs and navigation belong to the workspace, not this utility
            // window. Release its lease before updating the originating window.
            // Only dialog/navigation actions activate it; ordinary edits stay here.
            let action = action.clone();
            cx.defer(move |cx| {
                let _ = owner.update(cx, |_, window, cx| {
                    desk.update(cx, |desk, cx| action(desk, window, cx));
                });
            });
        } else {
            desk.update(cx, |desk, cx| action(desk, window, cx));
        }
    })
}

pub(super) fn toggle(
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

pub(super) fn stepper(
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
pub(super) fn resets<T: Clone + PartialEq + 'static, F>(
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
pub(super) fn fact(title: impl Into<SharedString>, value: String) -> SettingItem {
    SettingItem::new(
        title,
        SettingField::render(move |_, _, _| div().text_sm().child(value.clone())),
    )
    .information()
}

/// A slider with its value beside it, and a reset to the shipped default.
pub(super) fn slider_item(
    controls: Option<&SettingsControls>,
    handle: &WeakEntity<Desk>,
    knob: Knob,
    description: impl Into<String>,
) -> Option<SettingItem> {
    let slider = controls?
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
    Some(
        SettingItem::new(knob.title, field)
            .wide()
            .description(description.into()),
    )
}

/// `text`, then where the value came from and when an edit to that source applies.
pub(super) fn sourced(
    desk: &Desk,
    text: &str,
    ghostty_keys: &[&str],
    field: fn(&DisplayPrefs) -> String,
) -> String {
    let source = settings::display_provenance(&desk.prefs, desk.ghostty(), ghostty_keys, field);
    notes([text, &source.label(), source.applies().label()])
}

/// Non-empty notes joined into one description line.
pub(super) fn notes<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    parts
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}
