//! Settings has its own quiet surface and navigation, not a second palette.

use rune::{Color, El, TextStyle, col, el, row, text, text_input};

use super::{CAPTION, Ink, icons, pressable, updates};
use crate::desk::{Desk, SettingsCategory as Category};

mod controls;
mod entries;

pub(super) use controls::{section, toggle};

const TITLE: TextStyle = TextStyle::new(21.0, 650.0);
const COPY: TextStyle = TextStyle::new(13.0, 450.0).lh(18.0);

pub(super) fn primary(desk: &Desk, desktop: TextStyle) -> TextStyle {
    if desk.phone.is_some() {
        TextStyle {
            size: 17.0,
            line_height: 22.0,
            ..desktop
        }
    } else {
        desktop
    }
}

pub(super) fn secondary(desk: &Desk, desktop: TextStyle) -> TextStyle {
    if desk.phone.is_some() {
        TextStyle {
            size: 13.0,
            line_height: 18.0,
            ..desktop
        }
    } else {
        desktop
    }
}

fn surface(ink: &Ink) -> Color {
    if crate::theme::on_fill(ink.bg) == Color::WHITE {
        ink.raised
    } else {
        ink.bg
    }
}

fn search(desk: &Desk, ink: &Ink) -> El {
    let style = primary(desk, COPY);
    let field = text_input(&desk.settings_ui.query, style)
        .key("settings-query")
        .label("Search Settings")
        .single_line()
        .min_h(44.0)
        // Native input lays out from its top inset, not the row's alignment.
        // Center the 22-point phone or 18-point desktop line in the full target.
        .py((44.0 - style.line_px()) / 2.0)
        .grow(1.0)
        .min_w(0.0)
        .color(ink.text)
        .placeholder("Search Settings", ink.dim)
        .on_edit(|d: &mut Desk| &mut d.settings_ui.query)
        .on_input_key(Desk::settings_field_key);
    row()
        .w_full()
        .min_h(44.0)
        .pl(12.0)
        .gap(8.0)
        .items_center()
        .radius(12.0)
        .fill(surface(ink))
        .border(1.0, ink.separator)
        .child(
            rune::icon(&icons::SEARCH, 16.0)
                .color(ink.dim)
                .a11y_hidden(),
        )
        .child(field)
        .children((!desk.settings_ui.query.text.is_empty()).then(|| {
            pressable("settings-clear", ink)
                .size(44.0, 44.0)
                .focusable(true)
                .preserve_focus_on_press(true)
                .label("Clear search")
                .center()
                .on_press(Desk::clear_settings_search)
                .child(rune::icon(&icons::CLOSE, 16.0).color(ink.dim).a11y_hidden())
        }))
        .pr(4.0)
}

fn category_icon(category: Category, ink: &Ink) -> El {
    let (glyph, hue) = match category {
        Category::Appearance => (&icons::SIDEBAR, 275.0),
        Category::Terminal => (&icons::PROMPT, 160.0),
        Category::Notifications => (&icons::INBOX, 25.0),
        Category::RemoteHosts => (&icons::PAD, 240.0),
        Category::Developer => (&icons::KEYBOARD, 310.0),
        Category::About => (&icons::MORE, 210.0),
    };
    // Only these small identifiers carry colour, never the cards or theme list.
    let tint = rune::oklch(0.57, 0.13, hue);
    el().size(28.0, 28.0)
        .radius(8.0)
        .fill(tint.alpha(0.13))
        .center()
        .child(rune::icon(glyph, 16.0).color(ink.text))
        .a11y_hidden()
}

fn category_row(category: Category, desk: &Desk, ink: &Ink, phone: bool) -> El {
    let selected = desk.settings_ui.category.unwrap_or(Category::Appearance) == category;
    let mut item = pressable(category.key(), ink)
        .w_full()
        .min_h(if phone { 56.0 } else { 48.0 })
        .py(if phone { 8.0 } else { 0.0 })
        .px(12.0)
        .gap(10.0)
        .items_center()
        .radius(10.0)
        .focusable(true)
        .role("button")
        .label(category.name())
        .on_press(move |d: &mut Desk| d.settings_category(category))
        .child(category_icon(category, ink))
        .child(category_copy(category, desk, ink, phone));
    if phone {
        item = item
            .a11y_value(entries::category_value(category, desk))
            .child(
                rune::icon(&icons::CHEVRON_RIGHT, 14.0)
                    .color(ink.dim)
                    .a11y_hidden(),
            );
    } else if selected {
        item = item.fill(ink.current).focus_selected(true);
    }
    item
}

fn category_copy(category: Category, desk: &Desk, ink: &Ink, phone: bool) -> El {
    let title = text(category.name())
        .font(primary(desk, COPY))
        .color(ink.text);
    if !phone {
        return title.grow(1.0).a11y_hidden();
    }
    row()
        .grow(1.0)
        .min_w(0.0)
        .gap(8.0)
        .flex_wrap()
        .items_center()
        .child(title.w(100.0).min_w(100.0).grow(1.0).wrap())
        .child(
            text(entries::category_value(category, desk))
                .font(secondary(desk, CAPTION))
                .color(ink.dim)
                .max_w(140.0)
                .wrap_anywhere(),
        )
        .a11y_hidden()
}

fn categories(desk: &Desk, ink: &Ink) -> El {
    let groups: [&[Category]; 3] = [
        &[
            Category::Appearance,
            Category::Terminal,
            Category::Notifications,
        ],
        &[Category::RemoteHosts],
        &[Category::Developer, Category::About],
    ];
    col()
        .w_full()
        .gap(24.0)
        .children(groups.into_iter().map(|group| {
            controls::card(
                group
                    .iter()
                    .map(|c| category_row(*c, desk, ink, true))
                    .collect(),
                ink,
            )
        }))
}

fn detail(desk: &Desk, ink: &Ink) -> El {
    let query = desk.settings_ui.query.text.trim();
    if query.is_empty() && desk.phone.is_some() && desk.settings_ui.category.is_none() {
        return categories(desk, ink);
    }
    let groups = entries::matching(desk);
    let mut body = col().w_full().gap(24.0);
    if groups.is_empty() {
        body = body.child(
            col()
                .key("settings-no-results")
                .w_full()
                .py(32.0)
                .gap(8.0)
                .child(
                    text("No settings found")
                        .font(primary(desk, COPY))
                        .color(ink.text),
                )
                .child(
                    text("Try a name, category or current value.")
                        .font(secondary(desk, CAPTION))
                        .color(ink.dim)
                        .wrap(),
                ),
        );
    }
    for (category, entries) in groups {
        let rows = entries
            .into_iter()
            .map(|entry| entry.view(desk, ink))
            .collect();
        body = body.child(
            col()
                .w_full()
                .gap(8.0)
                .child(
                    section(category.name(), ink)
                        .font(secondary(desk, TextStyle::new(12.0, 600.0)))
                        .px(16.0),
                )
                .child(controls::card(rows, ink)),
        );
        if category == Category::Appearance && query.is_empty() {
            body = body.child(controls::preview(desk, ink));
        }
    }
    body
}

fn close(ink: &Ink) -> El {
    pressable("settings-close", ink)
        .size(44.0, 44.0)
        .radius(22.0)
        .fill(ink.current)
        .center()
        .focusable(true)
        .label("Close")
        .on_press(Desk::close_settings)
        .child(
            rune::icon(&icons::CLOSE, 18.0)
                .color(ink.text)
                .a11y_hidden(),
        )
}

fn heading(desk: &Desk, ink: &Ink) -> El {
    if desk.phone.is_some() {
        return phone_heading(desk, ink);
    }
    row()
        .w_full()
        .h(52.0)
        .px(16.0)
        .gap(10.0)
        .items_center()
        .child(
            text("Settings")
                .font(TITLE)
                .color(ink.text)
                .role("heading")
                .level(1),
        )
        .child(el().grow(1.0))
        .child(close(ink))
}

fn phone_heading(desk: &Desk, ink: &Ink) -> El {
    let back = desk.phone.is_some() && desk.settings_ui.category.is_some();
    row()
        .w_full()
        .h(64.0)
        .px(16.0)
        .gap(10.0)
        .items_center()
        .child(if back {
            pressable("settings-back", ink)
                .size(44.0, 44.0)
                .focusable(true)
                .label("Back to Settings")
                .center()
                .on_press(Desk::settings_back)
                .child(
                    rune::icon(&icons::CHEVRON_LEFT, 20.0)
                        .color(ink.text)
                        .a11y_hidden(),
                )
        } else {
            close(ink)
        })
        .child(
            el().grow(1.0).center().child(
                text("Settings")
                    .font(TITLE)
                    .color(ink.text)
                    .role("heading")
                    .level(1),
            ),
        )
        .child(if back {
            close(ink)
        } else {
            el().size(44.0, 44.0)
        })
}

/// A quieter top edge below the heading, and a stronger cue toward more
/// content below. Keyboard-revealed controls stop clear of either fade.
fn overflow_fade() -> rune::ScrollFade {
    rune::ScrollFade::strength(16.0).with_end(rune::ScrollFade::strength(24.0).linear())
}

fn scroller(desk: &Desk, ink: &Ink) -> El {
    col()
        .key("settings-content")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .min_w(0.0)
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .scroll_fade(overflow_fade())
        .scroll_margin(24.0)
        .child(
            col().w_full().padding(16.0).gap(16.0)
            // A search must never conceal an update failure or protocol warning.
            .children(updates::warnings(desk, ink))
            .child(detail(desk, ink)),
        )
}

fn phone(desk: &Desk, ink: &Ink) -> El {
    col()
        .key("settings-scrim")
        .w_full()
        .h_full()
        .min_h(0.0)
        .modal(true, Desk::settings_escape)
        .label("Settings")
        .fill(ink.sidebar)
        .child(
            col()
                .key("settings")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .radius(24.0)
                .clip_rounded(24.0)
                .fill(ink.sidebar)
                .child(heading(desk, ink))
                .child(scroller(desk, ink))
                .child(el().w_full().padding(12.0).child(search(desk, ink))),
        )
}

fn desktop(desk: &Desk, ink: &Ink) -> El {
    let rail = col()
        .key("settings-navigation")
        .w(190.0)
        .h_full()
        .min_h(0.0)
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .scroll_fade(overflow_fade())
        .scroll_margin(24.0)
        .padding(10.0)
        .gap(4.0)
        .role("navigation")
        .label("Settings categories")
        .children(
            Category::ALL
                .into_iter()
                .map(|c| category_row(c, desk, ink, false)),
        );
    let panel = col()
        .key("settings")
        .w_full()
        .max_w(840.0)
        .h_full()
        .max_h(600.0)
        .min_h(0.0)
        .radius(16.0)
        .clip_rounded(16.0)
        .fill(ink.sidebar)
        .border(1.0, ink.separator)
        .shadow(18.0, 56.0, ink.shade)
        .child(heading(desk, ink))
        .child(
            row()
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .child(rail)
                .child(el().w(1.0).h_full().fill(ink.separator))
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .h_full()
                        .child(el().w_full().px(16.0).pb(8.0).child(search(desk, ink)))
                        .child(scroller(desk, ink)),
                ),
        );
    col()
        .key("settings-scrim")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .padding(32.0)
        .center()
        .fill(Color::BLACK.alpha(0.20))
        .modal(true, Desk::settings_escape)
        .label("Settings")
        .child(panel)
}

pub fn view(desk: &Desk, ink: &Ink) -> El {
    match desk.phone {
        Some(_) => phone(desk, ink),
        None => desktop(desk, ink),
    }
}

#[cfg(test)]
mod tests;
