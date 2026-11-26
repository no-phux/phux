//! Settings has its own quiet surface and navigation, not a second palette.
//!
//! The desktop form is the OWN sheet at 840×600 with a category rail; the
//! phone form is full bleed at `radius.xl`. Everything is drawn
//! from the window's [`Tokens`].

use super::tokens::layout::SETTINGS as GEOMETRY;
use crate::ui::tokens::Voice;
use rune::{El, TextStyle, col, el, row, text, text_input};
use rune_kit::{IconButton, IconButtonVariant};

use super::tokens::{Paint, Tier, Tokens};
use super::{icons, pressable, updates};
use crate::desk::{Desk, SettingsCategory as Category};

mod controls;
mod entries;
mod theme_row;

pub(super) use controls::{Toggle, grown, kit_button, section, toggle};

/// Primary copy: a row's title. The phone reads a size up, `lead` for
/// `body` (the phone type bumps onto roles).
pub(super) fn primary(desk: &Desk, t: &Tokens) -> TextStyle {
    if desk.phone.is_some() {
        t.ty.lead
    } else {
        t.ty.body
    }
}

/// Secondary copy: a row's detail or value; `body` on the phone for
/// `caption`.
pub(super) fn secondary(desk: &Desk, t: &Tokens) -> TextStyle {
    if desk.phone.is_some() {
        t.ty.body
    } else {
        t.ty.caption
    }
}

/// A section's heading type: `label` a weight up.
pub(super) fn heading_type(desk: &Desk, t: &Tokens) -> TextStyle {
    let base = if desk.phone.is_some() {
        t.ty.body
    } else {
        t.ty.label
    };
    base.semibold()
}

/// The ground Settings' cards sit on: content lifted off the sheet.
fn surface(t: &Tokens) -> rune::Color {
    t.roles.layer.raised.paint()
}

/// The search field, styled like a kit `TextInput` (control outline,
/// `radius.sm`) around Rune's native editor, which keeps the app-owned
/// history and the 44 pt target the phone needs.
fn search(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    let style = primary(desk, t);
    let field = text_input(&desk.settings_ui.query, style)
        .key("settings-query")
        .label("Search settings")
        .single_line()
        .min_h(s.touch_min)
        // Native input lays out from its top inset, not the row's alignment.
        // Center the line in the full target.
        .py((s.touch_min - style.line_px()) / 2.0)
        .grow(1.0)
        .min_w(0.0)
        .color(t.roles.text.text.paint())
        .placeholder("Search settings", t.roles.text.faint.paint())
        .on_edit(|d: &mut Desk| &mut d.settings_ui.query)
        .on_input_key(Desk::settings_field_key)
        .on_input_change(|d: &mut Desk, tx| d.settings_search_edited(tx));
    row()
        .w_full()
        .min_h(s.touch_min)
        .pl(s.s3)
        .pr(s.s1)
        .gap(s.s2)
        .items_center()
        .radius(t.radius.sm)
        .fill(surface(t))
        .border(s.hairline, t.roles.border.control.paint())
        .child(
            rune::icon(&icons::SEARCH, t.icon.grid)
                .color(t.roles.text.dim.paint())
                .a11y_hidden(),
        )
        .child(field)
        .children((!desk.settings_ui.query.text.is_empty()).then(|| {
            IconButton::new(&icons::CLOSE)
                .variant(IconButtonVariant::Ghost)
                .label("Clear search")
                .on_press_tx(Desk::clear_settings_search)
                .build(&t.kit)
                .key("settings-clear")
                .min_w(s.touch_min)
                .min_h(s.touch_min)
                .preserve_focus_on_press(true)
        }))
}

/// A category's identifier: a neutral tile (`wash.current`) with the icon
/// in `text.dim`. Colour means state or identity, never decoration (D8).
fn category_icon(category: Category, t: &Tokens) -> El {
    let glyph = match category {
        Category::Appearance => &icons::SIDEBAR,
        Category::Terminal => &icons::PROMPT,
        Category::Notifications => &icons::INBOX,
        Category::RemoteHosts => &icons::PAD,
        Category::Developer => &icons::KEYBOARD,
        Category::Labs => &icons::BOLT,
        Category::About => &icons::MORE,
    };
    let side = t.icon.button.md;
    el().size(side, side)
        .radius(t.radius.sm)
        .fill(t.roles.wash.current.paint())
        .center()
        .child(rune::icon(glyph, t.icon.grid).color(t.roles.text.dim.paint()))
        .a11y_hidden()
}

fn category_row(category: Category, desk: &Desk, t: &Tokens, phone: bool) -> El {
    let s = &t.space;
    let selected = desk.settings_ui.category.unwrap_or(Category::Appearance) == category;
    let mut item = pressable(category.key(), t)
        .w_full()
        .min_h(if phone { s.s12 + s.s2 } else { s.s12 })
        .py(if phone { s.s2 } else { 0.0 })
        .px(s.s3)
        .gap(s.s3)
        .items_center()
        .radius(t.radius.sm)
        .focusable(true)
        .role("button")
        .label(category.name())
        .on_press(move |d: &mut Desk| d.settings_category(category))
        .child(category_icon(category, t))
        .child(category_copy(category, desk, t, phone));
    if phone {
        item = item
            .a11y_value(entries::category_value(category, desk))
            .child(
                rune::icon(&icons::CHEVRON_RIGHT, t.icon.size.md)
                    .color(t.roles.text.dim.paint())
                    .a11y_hidden(),
            );
    } else if selected {
        item = super::lifted(item, t).focus_selected(true);
    }
    item
}

fn category_copy(category: Category, desk: &Desk, t: &Tokens, phone: bool) -> El {
    let title = text(category.name())
        .font(primary(desk, t))
        .color(t.roles.text.text.paint());
    if !phone {
        return title.grow(1.0).a11y_hidden();
    }
    row()
        .grow(1.0)
        .min_w(0.0)
        .gap(t.space.s2)
        .flex_wrap()
        .items_center()
        .child(
            title
                .w(GEOMETRY.phone_title_min)
                .min_w(GEOMETRY.phone_title_min)
                .grow(1.0)
                .wrap(),
        )
        .child(
            text(entries::category_value(category, desk))
                .font(secondary(desk, t))
                .color(t.roles.text.dim.paint())
                .max_w(GEOMETRY.phone_value_max)
                .wrap_anywhere(),
        )
        .a11y_hidden()
}

fn categories(desk: &Desk, t: &Tokens) -> El {
    let last: &[Category] = if desk.developer_mode {
        &[Category::Developer, Category::Labs, Category::About]
    } else {
        &[Category::Developer, Category::About]
    };
    let groups: [&[Category]; 3] = [
        &[
            Category::Appearance,
            Category::Terminal,
            Category::Notifications,
        ],
        &[Category::RemoteHosts],
        last,
    ];
    col()
        .w_full()
        .gap(t.space.section)
        .children(groups.into_iter().map(|group| {
            controls::card(
                group
                    .iter()
                    .map(|c| category_row(*c, desk, t, true))
                    .collect(),
                t,
            )
        }))
}

fn detail(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    let query = desk.settings_ui.query.text.trim();
    if query.is_empty() && desk.phone.is_some() && desk.settings_ui.category.is_none() {
        return categories(desk, t);
    }
    let groups = entries::matching(desk);
    let mut body = col().w_full().gap(s.section);
    if groups.is_empty() {
        body = body.child(
            col()
                .key("settings-no-results")
                .w_full()
                .py(s.s8)
                .gap(s.s2)
                .child(
                    text("No settings found")
                        .font(primary(desk, t))
                        .color(t.roles.text.text.paint()),
                )
                .child(
                    text("Try a name, category or current value.")
                        .font(secondary(desk, t))
                        .color(t.roles.text.dim.paint())
                        .wrap(),
                ),
        );
    }
    for (category, entries) in groups {
        let rows = entries
            .into_iter()
            .map(|entry| entry.view(desk, t))
            .collect();
        body = body.child(
            col()
                .w_full()
                .gap(s.s2)
                .child(
                    section(category.name(), t)
                        .font(heading_type(desk, t))
                        .px(s.s4),
                )
                .child(controls::card(rows, t)),
        );
        if category == Category::Appearance && query.is_empty() {
            body = body.child(controls::preview(desk, t));
        }
    }
    body
}

/// The sheet's close: kit `IconButton`, ghost, as every sheet's is; no
/// rest fill (`wash.current` is the keyboard cursor's).
fn close(t: &Tokens) -> El {
    let side = t.space.touch_min;
    super::icon_press(
        &icons::CLOSE,
        "Close",
        "settings-close",
        t,
        Desk::close_settings,
    )
    .min_w(side)
    .min_h(side)
}

fn title(t: &Tokens) -> El {
    text("Settings")
        .font(t.ty.title)
        .color(t.roles.text.text.paint())
        .role("heading")
        .level(1)
}

fn heading(desk: &Desk, t: &Tokens) -> El {
    if desk.phone.is_some() {
        return phone_heading(desk, t);
    }
    let s = &t.space;
    row()
        .w_full()
        .min_h(s.s12)
        .px(s.s4)
        .py(s.s1)
        .gap(s.s3)
        .items_center()
        .child(title(t))
        .child(el().grow(1.0))
        .child(close(t))
}

fn phone_heading(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    let back = desk.phone.is_some() && desk.settings_ui.category.is_some();
    row()
        .w_full()
        .h(s.s16)
        .px(s.s4)
        .gap(s.s3)
        .items_center()
        .child(if back {
            pressable("settings-back", t)
                .size(s.touch_min, s.touch_min)
                .radius(t.radius.full)
                .focusable(true)
                .label("Back to Settings")
                .center()
                .on_press(Desk::settings_back)
                .child(
                    rune::icon(&icons::CHEVRON_LEFT, t.icon.size.lg)
                        .color(t.roles.text.text.paint())
                        .a11y_hidden(),
                )
        } else {
            close(t)
        })
        .child(el().grow(1.0).center().child(title(t)))
        .child(if back {
            close(t)
        } else {
            el().size(s.touch_min, s.touch_min)
        })
}

/// A quieter top edge below the heading, and a stronger cue toward more
/// content below. Keyboard-revealed controls stop clear of either fade.
fn overflow_fade(t: &Tokens) -> rune::ScrollFade {
    rune::ScrollFade::strength(t.space.s4).with_end(rune::ScrollFade::strength(t.space.s6).linear())
}

fn scroller(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    col()
        .key("settings-content")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .min_w(0.0)
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .scroll_fade(overflow_fade(t))
        .scroll_margin(s.s6)
        .child(
            col().w_full().padding(s.s4).gap(s.s4)
            // A search must never conceal an update failure or protocol warning.
            .children(updates::warnings(desk, t))
            .child(detail(desk, t)),
        )
}

fn phone(desk: &Desk, t: &Tokens) -> El {
    let ground = t.roles.layer.sidebar.paint();
    col()
        .key("settings-scrim")
        .w_full()
        .h_full()
        .min_h(0.0)
        .modal_tx(true, Desk::settings_escape)
        .label("Settings")
        .fill(ground)
        .child(
            col()
                .key("settings")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .radius(t.radius.xl)
                .clip_rounded(t.radius.xl)
                .fill(ground)
                .child(heading(desk, t))
                .child(scroller(desk, t))
                .child(el().w_full().padding(t.space.s3).child(search(desk, t))),
        )
}

fn desktop(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    let rule = t.roles.border.base.paint();
    let rail = col()
        .key("settings-navigation")
        .w(GEOMETRY.rail_w)
        .h_full()
        .min_h(0.0)
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .scroll_fade(overflow_fade(t))
        .scroll_margin(s.s6)
        .padding(s.s2)
        .gap(s.s1)
        .role("navigation")
        .label("Settings categories")
        .children(Category::shown(desk.developer_mode).map(|c| category_row(c, desk, t, false)));
    let panel = col()
        .key("settings")
        .w_full()
        .max_w(GEOMETRY.sheet_w)
        .h_full()
        .max_h(GEOMETRY.sheet_h)
        .min_h(0.0)
        .radius(t.radius.lg)
        .clip_rounded(t.radius.lg)
        .fill(t.roles.layer.sidebar.paint())
        .border(s.hairline, rule)
        .transition(t.enter())
        .child(heading(desk, t))
        .child(rune::rule(s.hairline, rule).w_full())
        .child(
            row()
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .child(rail)
                .child(el().w(s.hairline).h_full().fill(rule))
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .h_full()
                        .child(
                            el().w_full()
                                .px(s.s4)
                                .pt(s.s3)
                                .pb(s.s2)
                                .child(search(desk, t)),
                        )
                        .child(scroller(desk, t)),
                ),
        );
    // Clearing a search moves its results, so the dismissal needs the Tx.
    super::scrim(
        col().key("settings-scrim").padding(s.s8).center(),
        "Settings",
        None,
        t,
    )
    .modal_tx(true, Desk::settings_escape)
    .child(t.elevate(panel, Tier::Overlay))
}

pub fn view(desk: &Desk, p: &Tokens) -> El {
    let t = p;
    match desk.phone {
        Some(_) => phone(desk, t),
        None => desktop(desk, t),
    }
}

#[cfg(test)]
mod tests;
