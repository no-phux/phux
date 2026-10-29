//! The theme picker: every theme as a row with its colours, previewed across
//! the whole app while highlighted; Enter keeps it, Escape restores.

use gpui::prelude::*;
use gpui::{AnyElement, Context, Window, div, px};
use gpui_kit::component::command::{Command, CommandItem};
use gpui_kit::component::{ActiveTheme, Icon, IconName, IndexPath, Sizable};

use super::inputs::palette_state;
use super::sheet;
use crate::app::color::from_hex;
use crate::app::desk::Desk;
use crate::app::skin::Look;
use crate::settings::{self, theme};

/// Built-in themes, after the Ghostty one when the config names a theme.
pub(super) fn listed_themes(desk: &Desk) -> Vec<theme::Theme> {
    let mut themes = theme::themes();
    if let Some(extra) = settings::ghostty_theme(desk.ghostty()) {
        themes.insert(0, extra);
    }
    themes
}

pub(super) fn theme_panel(
    desk: &mut Desk,
    look: &Look,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let state = palette_state(desk, window, cx);
    let themes = listed_themes(desk);
    let original = desk
        .theme_pick
        .as_ref()
        .map_or_else(|| desk.prefs.theme_id.clone(), |(id, _)| id.clone());
    // The list opens on the theme in use, once its rows are installed.
    if let Some((_, placed)) = desk.theme_pick.as_mut()
        && !*placed
    {
        *placed = true;
        if let Some(row) = themes.iter().position(|theme| theme.id == original) {
            let state = state.clone();
            window.defer(cx, move |window, cx| {
                state.update(cx, |state, cx| {
                    state.set_selected_index(Some(IndexPath::new(row)), window, cx);
                });
            });
        }
    }
    let ids: Vec<String> = themes.iter().map(|theme| theme.id.clone()).collect();
    let rows = themes.into_iter().map(|theme| {
        let kept = theme.id == original;
        let name = theme.name.clone();
        let swatches = [
            theme.background.clone(),
            theme.surface.clone(),
            theme.foreground.clone(),
            theme.accent.clone(),
            theme.success.clone(),
            theme.warning.clone(),
            theme.danger.clone(),
        ];
        let tone = if theme.appearance == "light" {
            "Light"
        } else {
            "Dark"
        };
        CommandItem::new().label(name.clone()).child(move |_, cx| {
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .flex()
                        .flex_shrink_0()
                        .rounded(px(5.))
                        .overflow_hidden()
                        .border_1()
                        .border_color(cx.theme().border)
                        .children(
                            swatches
                                .iter()
                                .map(|hex| div().w(px(10.)).h(px(18.)).bg(from_hex(hex))),
                        ),
                )
                .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tone),
                )
                .child(div().w(px(14.)).children(kept.then(|| {
                    Icon::new(IconName::Check)
                        .small()
                        .text_color(cx.theme().muted_foreground)
                })))
        })
    });
    let preview_handle = cx.weak_entity();
    let keep_handle = preview_handle.clone();
    let preview_ids = ids.clone();
    let muted = from_hex(&look.chrome.muted);
    let command = Command::new(&state)
        .bordered(false)
        .placeholder("Select theme…")
        .max_h(px(420.))
        .items(rows)
        .footer(move |_, _, _| {
            div()
                .px(px(12.))
                .py(px(10.))
                .flex()
                .justify_end()
                .text_xs()
                .text_color(muted)
                .child("↑↓ preview · ↵ keep · esc restore")
        })
        .on_select(move |index: IndexPath, _, cx| {
            if let Some(id) = preview_ids.get(index.row).cloned() {
                let _ = preview_handle.update(cx, |desk, cx| desk.preview_theme(id, cx));
            }
        })
        .on_confirm(move |index: IndexPath, window, cx| {
            if let Some(id) = ids.get(index.row).cloned() {
                let _ = keep_handle.update(cx, |desk, cx| desk.keep_theme(id, window, cx));
            }
        });
    sheet(look, 520.)
        .overflow_hidden()
        .debug_selector(|| "theme-picker".into())
        .key_context("ThemePicker")
        .child(command)
        .into_any_element()
}
