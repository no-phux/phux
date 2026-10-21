//! Keyboard shortcuts, conflicts, and Ghostty configuration integration.

use super::catalog::{Content as SettingPage, Group as SettingGroup, Item as SettingItem};
use gpui::prelude::*;
use gpui::{WeakEntity, div, px};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::setting::SettingField;
use gpui_kit::component::{ActiveTheme, Disableable, IconName, Sizable};

use crate::settings;

use super::super::desk::Desk;
use super::{click_runs, notes, sourced, toggle};

pub(super) fn keyboard_page(desk: &Desk, handle: &WeakEntity<Desk>) -> SettingPage {
    let input = SettingGroup::new()
        .title("Input")
        .item(toggle(
            handle,
            "Option sends Alt",
            |prefs| &mut prefs.option_as_alt,
            sourced(
                desk,
                "Off keeps macOS Option characters such as å and ß.",
                &["macos-option-as-alt"],
                |prefs| prefs.option_as_alt.to_string(),
            ),
        ))
        .item(toggle(
            handle,
            "Use Ghostty keybinds",
            |prefs| &mut prefs.ghostty_keys,
            "Your Ghostty keybind lines override these shortcuts, and global: binds toggle the quick terminal.",
        ));
    let catalog = settings::builtin_commands();
    let shown = settings::shown_chords(desk.keymap(), &settings::default_chords());
    // (title, group, chord, command id)
    let mut rows: Vec<(String, String, String, String)> = catalog
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
                        command.id.to_owned(),
                    )
                })
        })
        .collect();
    // Ghostty binds with no catalog entry (send:, font-size:, …) are listed by their action.
    for (chord, id) in desk.keymap() {
        if !catalog.iter().any(|command| command.id == id) {
            rows.push((id.clone(), "Ghostty".to_owned(), chord.clone(), id.clone()));
        }
    }
    let shortcuts = SettingGroup::new()
        .title("Shortcuts")
        .items(rows.into_iter().map(|(title, group, chord, id)| {
            let shown = settings::display_chord(&chord);
            let keystroke = super::super::ui::keystroke(&chord);
            let source =
                settings::chord_source(desk.ghostty(), desk.prefs.ghostty_keys, &chord, &id);
            let applies = match source {
                settings::ChordSource::Ghostty(_) => settings::Applies::OnReload,
                settings::ChordSource::BuiltIn => settings::Applies::Live,
            };
            let item = SettingItem::new(
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
            .information()
            .keywords([group, settings::display_chord(&chord)]);
            // Built-in is every row's default; only a chord from elsewhere says so.
            match source {
                settings::ChordSource::BuiltIn => item,
                settings::ChordSource::Ghostty(_) => {
                    item.description(notes([source.label().as_str(), applies.label()]))
                }
            }
        }));
    let page = SettingPage::new("Keyboard")
        .icon(IconName::ALargeSmall)
        .group(input);
    match conflicts_group(desk) {
        Some(conflicts) => page.group(conflicts).group(shortcuts),
        None => page.group(shortcuts),
    }
}

/// Chords the Ghostty config binds to different actions, each with every
/// line that binds it. Hidden when there are none, and while Ghostty
/// keybinds are off, since then none of those lines apply.
fn conflicts_group(desk: &Desk) -> Option<SettingGroup> {
    if !desk.prefs.ghostty_keys {
        return None;
    }
    let conflicts = settings::keymap_conflicts(desk.ghostty());
    if conflicts.is_empty() {
        return None;
    }
    let title = |id: &str| {
        settings::builtin_commands()
            .iter()
            .find(|command| command.id == id)
            .map_or_else(|| id.to_owned(), |command| command.title.to_owned())
    };
    let items = conflicts.into_iter().map(|conflict| {
        let lines = conflict
            .binds
            .iter()
            .map(|bind| format!("{} at {}", title(&bind.command), bind.origin.label()))
            .collect::<Vec<_>>()
            .join("; ");
        let chord = settings::display_chord(&conflict.chord);
        let winner = conflict
            .binds
            .last()
            .map(|bind| title(&bind.command))
            .unwrap_or_default();
        let shown = winner.clone();
        SettingItem::new(
            chord.clone(),
            SettingField::render(move |_, _, cx| {
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(shown.clone())
            }),
        )
        .information()
        .description(format!(
            "Bound more than once: {lines}. The last line wins."
        ))
        .keywords(["conflict", "duplicate", "keybind", chord.as_str()].map(str::to_owned))
    });
    Some(
        SettingGroup::new()
            .title("Shortcut conflicts")
            .description("Your Ghostty config binds these chords to different actions.")
            .items(items),
    )
}

pub(super) fn ghostty_page(desk: &Desk, handle: &WeakEntity<Desk>) -> SettingPage {
    let found = desk.ghostty_found;
    let ignored = desk.ghostty().unmapped.len();
    let status = if found {
        "Found."
    } else {
        "No Ghostty config found."
    };
    let font = desk
        .ghostty()
        .font_family
        .as_ref()
        .map(|font| format!("Font {font}."));
    let ignored = (ignored > 0).then(|| {
        let (lines, are) = if ignored == 1 {
            ("line has", "is")
        } else {
            ("lines have", "are")
        };
        format!("{ignored} {lines} no phux equivalent and {are} ignored.")
    });
    let status = SettingItem::new("Ghostty config", SettingField::render(|_, _, _| div()))
        .description(
            [Some(status.to_owned()), font, ignored]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" "),
        );
    let (apply, reload) = (handle.clone(), handle.clone());
    let actions = [
        SettingItem::new(
            "Use Ghostty look",
            SettingField::render(move |_, _, _| {
                Button::new("ghostty-apply")
                    .label("Apply")
                    .primary()
                    .small()
                    .disabled(!found)
                    .on_click(click_runs(&apply, |desk, _, cx| desk.import_ghostty(cx)))
            }),
        )
        .description("Copies its theme, font, size, cell size and padding into these settings."),
        SettingItem::new(
            "Reload config",
            SettingField::render(move |_, _, _| {
                Button::new("ghostty-reload")
                    .label("Reload")
                    .small()
                    .on_click(click_runs(&reload, |desk, _, cx| desk.reload_ghostty(cx)))
            }),
        )
        .description("Reads the files again for keybinds and the Ghostty theme."),
    ];
    let page = SettingPage::new("Ghostty").icon(IconName::Frame).group(
        SettingGroup::new()
            .item(status.information())
            .items(actions),
    );
    let config = desk.ghostty();
    let page = match diagnostic_group(
        &config.diagnostics,
        "Skipped lines",
        "These lines could not be read. Every other line still applies.",
    ) {
        Some(group) => page.group(group),
        None => page,
    };
    match diagnostic_group(
        &config.unsupported,
        "Not used here",
        "Ghostty accepts these lines; this app cannot represent their values, so it leaves them out.",
    ) {
        Some(group) => page.group(group),
        None => page,
    }
}

/// One row per diagnostic: file and line, the line's text, and why it was
/// left out. `None` when there are none.
fn diagnostic_group(
    diagnostics: &[settings::Diagnostic],
    title: &'static str,
    description: &'static str,
) -> Option<SettingGroup> {
    if diagnostics.is_empty() {
        return None;
    }
    Some(
        SettingGroup::new()
            .title(title)
            .description(description)
            .items(diagnostics.iter().map(|diagnostic| {
                let text = diagnostic.text.clone();
                SettingItem::new(
                    diagnostic.origin.label(),
                    SettingField::render(move |_, _, cx| {
                        div()
                            .max_w(px(260.))
                            .truncate()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(text.clone())
                    }),
                )
                .information()
                .description(diagnostic.reason)
                .keywords(["skipped", "malformed", "unsupported", "ghostty"].map(str::to_owned))
            })),
    )
}
