//! The command palette: a field over ranked rows in sections, on one
//! column grid; and the context menu, the kit's.
//!
//! On a desktop it floats as a glass panel under the window's top. On a
//! phone it is a page, as the Sessions page is: a search field with
//! Cancel over grouped cards, filling the room above the keyboard.

use std::ops::Range;

use crate::ui::tokens::Voice;
use rune::{El, col, el, row, text};

use super::icons;
use super::overlay::{EXTENTS, Glass, empty_note, glass, key_hint, pop};
use super::tokens::{Paint, Tier, Tokens, Type};
use crate::desk::Desk;
use crate::palette::{Hit, Mode};
use crate::ui::Cover;

mod header;
mod marks;
mod menu;
mod rows;
use header::{desktop_header, phone_header, verb_and_glyph};
pub use menu::context_menu;
use rows::Rows;
#[cfg(test)]
use rows::details_keys;

/// The palette's one column grid. Every leading edge (the field's glyph,
/// the rows' marks, the section headings, the footer's keys) sits
/// [`Grid::lead`] in from the panel or page; every text (the query, the
/// rows' titles) at [`Grid::text`].
#[derive(Clone, Copy)]
pub(super) struct Grid {
    /// The list's inset: from the panel on a desktop, from the page's edge
    /// to a card's rows on a phone.
    pub inset: f32,
    /// A row's own padding before its mark.
    pub pad: f32,
    /// The marks' column.
    pub slot: f32,
    /// Between a mark and its title.
    pub gap: f32,
    pub row_h: f32,
    /// The form's type roles: a phone's are its own.
    pub ty: Type,
    pub phone: bool,
}

impl Grid {
    fn new(t: &Tokens, phone: bool) -> Self {
        let s = &t.space;
        if phone {
            Self {
                // A card's rows run to its edges, as an iOS grouped list's
                // cells do: the row pads its mark in.
                inset: super::tokens::phone::page_margin(),
                pad: s.s3 + s.s1,
                // The Sessions page's leading column: its step up to the
                // grid's text cap, then as wide as the grown mark and a step.
                slot: ((s.s5 + s.s1) * t.type_scale.min(crate::desk::accessibility::GRID_CAP))
                    .max(t.icon.size.md + s.s1),
                gap: s.s2,
                row_h: s.row_h_touch,
                ty: super::tokens::phone::ty(t.type_scale),
                phone,
            }
        } else {
            Self {
                inset: s.s2,
                pad: s.s2,
                slot: s.s6,
                gap: s.s3,
                row_h: s.s8,
                ty: t.ty,
                phone,
            }
        }
    }

    /// The marks' column, from the panel's (or page's) edge.
    pub(super) fn lead(&self) -> f32 {
        self.inset + self.pad
    }

    /// The titles' column, from the panel's (or page's) edge.
    pub(super) fn text(&self) -> f32 {
        self.lead() + self.slot + self.gap
    }

    /// Where the list's rows are inset from: the panel on a desktop, the
    /// page's margin on a phone.
    fn list_edge(&self) -> f32 {
        if self.phone {
            super::tokens::phone::page_margin()
        } else {
            self.inset
        }
    }

    /// The marks' column cell for `mark`: centred on a desktop; on a phone
    /// at the column's leading edge, as the Sessions page sets its marks.
    pub(super) fn mark_cell(&self, key: String, mark: El) -> El {
        if self.phone {
            row().key(key).w(self.slot).items_center().child(mark)
        } else {
            el().key(key).w(self.slot).center().child(mark)
        }
    }

    /// A row's corners, nested in what holds it by the list's inset. A
    /// phone's rows are square: their card's corners clip them.
    pub(super) fn row_radius(&self, t: &Tokens) -> f32 {
        if self.phone {
            0.0
        } else {
            t.nested(t.radius.lg, self.inset)
        }
    }
}

/// A section's heading on the marks' column, never a row the arrows land
/// on. A desktop's is a quiet caption over its flat rows; a phone's is set
/// as the Sessions page sets a section's name over its card. Needs you
/// counts its rows in its tone, with its mark, as that page's heading does.
fn heading(title: &'static str, rows: Range<usize>, g: &Grid, t: &Tokens) -> El {
    let s = &t.space;
    let at = rows.start;
    let count = (title == crate::palette::sections::NEEDS_YOU).then(|| rows.len());
    let label = count.map_or_else(|| title.to_owned(), |n| format!("{title}, {n}"));
    let needs = t.status.state(phux_design::State::NeedsYou);
    let (font, ink) = if g.phone {
        (g.ty.lead.semibold(), t.roles.text.text.paint())
    } else {
        (g.ty.caption.semibold(), t.roles.text.dim.paint())
    };
    let name = text(title).font(font).color(ink).grow(1.0).min_w(0.0);
    // Whole, wrapped, at the accessibility sizes, as the Sessions page
    // sets a section's name; one line otherwise.
    let name = if g.phone && t.type_scale >= super::tokens::layout::PHONE.stacked_title_scale {
        name.wrap()
    } else {
        name.truncate()
    };
    row()
        .key(format!("palette-section-{at}"))
        .w_full()
        .pl(g.pad)
        .pr(g.pad)
        .pt(heading_top(at, g, t))
        .pb(s.s1)
        .gap(s.s2)
        .items_center()
        .role("heading")
        .label(label)
        .child(name.a11y_hidden())
        .children(count.map(|n| {
            row()
                .key("palette-needs-you-count")
                .gap(s.s1)
                .items_center()
                .a11y_hidden()
                .children(
                    g.phone
                        .then(|| rune::icon(needs.icon, t.icon.size.xs).color(needs.tone.ink)),
                )
                .child(
                    text(n.to_string())
                        .font(g.ty.caption.semibold().figures())
                        .color(needs.tone.ink),
                )
        }))
}

/// The phone's grouped cards and search bar, as the Sessions page fills
/// its own: one step up from the page, so a card reads as a card on dark
/// as it does on light.
pub(super) fn card_fill(t: &Tokens) -> rune::Color {
    if t.dark() {
        t.roles.layer.overlay.paint()
    } else {
        t.roles.layer.raised.paint()
    }
}

/// The list's runs: each heading's title and the rows under it, or one
/// untitled run when the list has no sections.
fn runs(
    headings: &[(usize, &'static str)],
    len: usize,
) -> Vec<(Option<&'static str>, Range<usize>)> {
    if headings.is_empty() {
        return vec![(None, 0..len)];
    }
    let ends = headings.iter().skip(1).map(|(at, _)| *at).chain([len]);
    headings
        .iter()
        .zip(ends)
        .map(|(&(at, title), end)| (Some(title), at..end))
        .collect()
}

/// A phone's run of rows: one grouped card, a hairline between rows from
/// the titles' column to the card's trailing edge, as an iOS grouped
/// list's separators run. A picked row fills to the card's edges, its
/// corners the card's.
fn card(rows: &Rows<'_>, hits: &[Hit], range: Range<usize>) -> El {
    let t = rows.t;
    let s = &t.space;
    let g = &rows.grid;
    let mut card = col()
        .w_full()
        .radius(t.radius.xl)
        .continuous_corners()
        .clip_rounded(t.radius.xl)
        .fill(card_fill(t));
    let first = range.start;
    for index in range {
        if index > first {
            card = card.child(
                el().w_full()
                    .pl(g.pad + g.slot + g.gap)
                    .child(
                        el().w_full()
                            .h(s.hairline)
                            .fill(t.roles.border.muted.paint()),
                    )
                    .a11y_hidden(),
            );
        }
        card = card.child(rows.row(&hits[index], index));
    }
    card
}

/// The ranked rows in their sections: flat under headings on a desktop,
/// a card per section on a phone.
fn list(rows: &Rows<'_>, hits: &[Hit], headings: &[(usize, &'static str)]) -> El {
    let t = rows.t;
    let g = &rows.grid;
    let mut results = col()
        .key("palette-results")
        .role("listbox")
        .label("Results")
        .focus_group()
        .w_full();
    results = if g.phone {
        let margin = super::tokens::phone::page_margin();
        // An unsectioned list (Themes) keeps a heading's room above it.
        let top = if headings.is_empty() {
            t.space.s4
        } else {
            t.space.s1
        };
        results.px(margin).pt(top).pb(t.space.s4).gap(t.space.s1)
    } else {
        results.padding(g.inset)
    };
    for (title, range) in runs(headings, hits.len()) {
        if let Some(title) = title {
            results = results.child(heading(title, range.clone(), g, t));
        }
        if g.phone {
            results = results.child(card(rows, hits, range));
        } else {
            for index in range {
                results = results.child(rows.row(&hits[index], index));
            }
        }
    }
    results
}

/// What stands in the list when it has no rows, or a note under them.
fn notes(desk: &Desk, results: El, hits: &[Hit], g: &Grid, t: &Tokens) -> El {
    let search = desk.connection.paths.as_ref();
    let query = desk.palette_query.text.trim();
    if hits.is_empty() && search.is_none() && !query.is_empty() {
        return results.child(no_results(query, desk.palette_mode, g, t));
    }
    if hits.is_empty() {
        let empty = match search {
            Some(s) if s.searching() => "Searching…",
            Some(s) if s.note.is_some() => s.note.as_deref().unwrap_or_default(),
            _ if desk.palette_mode == Mode::Screens => {
                desk.connection.resources.screens.empty_text()
            }
            _ => "Nothing matches",
        };
        return results.child(empty_note(empty, t));
    }
    match search.and_then(|s| s.note.clone()) {
        Some(note) => results.child(
            el().w_full()
                .pl(g.text() - g.list_edge())
                .pr(g.pad)
                .py(t.space.s1)
                .child(
                    text(note)
                        .font(g.ty.caption)
                        .color(t.roles.text.dim.paint()),
                ),
        ),
        None => results,
    }
}

fn rows<'a>(desk: &'a Desk, t: &'a Tokens, g: Grid, hits: &[Hit], bare_themes: bool) -> Rows<'a> {
    let current_terminal = desk.connection.shown.as_ref().map(|s| s.resource.clone());
    Rows {
        desk,
        t,
        grid: g,
        picked: desk.palette_picked.min(hits.len().saturating_sub(1)),
        wash: desk.palette_pick_shown(),
        ground: if g.phone {
            card_fill(t)
        } else {
            t.roles.layer.overlay.paint()
        },
        bare_themes,
        shown: desk.addressable(),
        current_terminal,
        current_session: desk.selected_session_name(),
        kept_theme: desk.kept_theme().map(|theme| theme.id),
        checks: hits.iter().any(|hit| rows::can_be_current(&hit.item)),
    }
}

/// The list that scrolls: up to its cap on a desktop, the page's height
/// on a phone. A row cut by the edge fades rather than slicing mid-glyph,
/// and says more lies below.
fn scroller(content: El, g: &Grid, cap: f32, t: &Tokens) -> El {
    let list = col()
        .key("palette-scroll")
        .w_full()
        .shrink()
        .min_h(0.0)
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .child(content);
    if g.phone {
        list.grow(1.0).scroll_edge_fade(t.space.s8)
    } else {
        // The fade spans the half row the cap leaves, so it reads as a
        // row going on, not a smudge.
        list.max_h(cap).scroll_edge_fade(t.space.s4)
    }
}

/// The desktop list's height: `base`, moved up to the middle of the row
/// it would end in, so half a row always says more lies below, whatever
/// the headings above it took.
fn half_row_cap(
    base: f32,
    hits: &[Hit],
    headings: &[(usize, &'static str)],
    g: &Grid,
    t: &Tokens,
) -> f32 {
    let s = &t.space;
    let row_h = g.row_h.max(g.ty.body.line_px() + 2.0 * s.s1);
    let mut y = g.inset;
    let mut last_mid = None;
    for (title, range) in runs(headings, hits.len()) {
        if title.is_some() {
            y += heading_top(range.start, g, t) + g.ty.caption.line_px() + s.s1;
        }
        for _ in range {
            let mid = y + row_h / 2.0;
            if y + row_h > base {
                // A heading the cut would split gives way to the row above.
                return if mid <= base {
                    mid
                } else {
                    last_mid.unwrap_or(base)
                };
            }
            last_mid = Some(mid);
            y += row_h;
        }
    }
    base
}

/// The room over a section's heading: less over the list's first.
fn heading_top(at: usize, g: &Grid, t: &Tokens) -> f32 {
    if g.phone || at > 0 {
        t.space.s3
    } else {
        t.space.s1
    }
}

/// `top` is the room above the panel (a phone's top inset); a short window
/// scrolls its rows.
pub fn view(desk: &Desk, p: &Tokens, top: f32) -> El {
    let t = p;
    let s = &t.space;
    let r = &t.roles;
    let phone = desk.phone.is_some();
    let mode = desk.palette_mode;
    let g = Grid::new(t, phone);
    let hits = desk.palette_hits();
    // Counted before the list is cut to its rows.
    let found = hits.len();
    let hits = &hits[..found.min(crate::palette::ROWS)];
    let headings = desk.palette_headings(hits);
    let bare = mode == Mode::Themes || !headings.is_empty();
    let rows = rows(desk, t, g, hits, bare);
    let results = notes(desk, list(&rows, hits, &headings), hits, &g, t);
    let (width, cap) = if mode == Mode::Paths {
        (EXTENTS.paths_w, EXTENTS.paths_rows_h)
    } else {
        (EXTENTS.palette_w, EXTENTS.palette_rows_h)
    };
    let cap = half_row_cap(cap, hits, &headings, &g, t);
    let rule = |key: &'static str| {
        rune::rule(s.hairline, r.border.base.paint())
            .w_full()
            .key(key)
    };
    let scrim = el()
        .key("palette-scrim")
        .cover()
        .on_layout(|d: &mut Desk, rect| d.pointer.window = [rect[2], rect[3]])
        .transition(rune::Transition::opacity())
        .modal(true, |d: &mut Desk| d.close_palette());
    if phone {
        let page = col()
            .key("palette")
            .w_full()
            .h_full()
            .pt(top)
            .fill(r.layer.ground.paint())
            .transition(rune::Transition::opacity())
            .child(phone_header(desk, t, &g))
            .child(rule("palette-header-rule"))
            .child(super::phone::swipe_list(
                desk,
                scroller(results, &g, cap, t),
                crate::desk::phone::dismiss::Away::Palette,
            ));
        // A swipe down, from its top, lowers it as Cancel does.
        let away = crate::desk::phone::dismiss::Away::Palette;
        return scrim.child(super::phone::swipeable(desk, page, away, t));
    }
    let (verb, _) = verb_and_glyph(mode);
    let panel = col()
        .key("palette")
        // A narrow window takes what it has.
        .w_full()
        .max_w(width)
        .shrink()
        .min_h(0.0)
        .radius(t.radius.lg)
        .clip_rounded(t.radius.lg)
        .border(s.hairline, r.border.base.paint())
        .transition(pop(t))
        .child(desktop_header(desk, t, &g, found))
        .child(rule("palette-header-rule"))
        .child(scroller(results, &g, cap, t))
        .child(rule("palette-footer-rule"))
        .child(
            row()
                .key("palette-footer")
                .w_full()
                .px(g.lead())
                .py(s.s2)
                .gap(s.s4)
                .items_center()
                // Nothing to move to or open: only Escape does anything.
                .children((!hits.is_empty()).then(|| key_hint("↑↓", "move", t)))
                .children((!hits.is_empty()).then(|| key_hint("↩", verb, t)))
                .child(key_hint("esc", "close", t)),
        );
    let panel = t.elevate(
        glass(panel, t, r.layer.overlay, Glass::Overlay, false),
        Tier::Overlay,
    );
    // Paths open where they'll land.
    if mode == Mode::Paths
        && let Some([x, y]) = desk.cursor_point()
    {
        return scrim.child(at_cursor(desk, panel, x, y, t));
    }
    scrim.fill(r.layer.scrim.paint()).child(
        col()
            .w_full()
            .h_full()
            .pt(desktop_top(desk))
            .px(s.s3)
            .pb(s.s3)
            .items_center()
            .child(panel),
    )
}

/// The room above the desktop panel: its usual drop, or a share of a short
/// window, as Spotlight sits a fifth of the way down.
fn desktop_top(desk: &Desk) -> f32 {
    let [_, window_h] = desk.pointer.window;
    let top = EXTENTS.palette_top;
    if window_h <= 0.0 {
        return top;
    }
    top.min(window_h * EXTENTS.palette_top_share)
        .max(EXTENTS.palette_top_min)
}

/// The path picker under the cursor's line, or above it where there is no
/// room below.
fn at_cursor(desk: &Desk, panel: El, x: f32, y: f32, t: &Tokens) -> El {
    let [window_w, window_h] = desk.pointer.window;
    let gap = t.space.s1;
    let below = y + gap;
    let tall = EXTENTS.paths_h;
    let y = if window_h > 0.0 && below + tall > window_h {
        (y - desk.metrics().cell_h - tall - gap).max(0.0)
    } else {
        below
    };
    panel.absolute(fit(x, EXTENTS.paths_w, window_w, gap), y)
}

/// What a mode lists, for the hint under a search that found nothing.
const fn try_hint(mode: Mode) -> &'static str {
    match mode {
        Mode::All => "Try a command, session or terminal name.",
        Mode::Sessions => "Try another session name, or Return to make it.",
        Mode::Themes => "Try a theme name.",
        Mode::Shortcuts | Mode::Essentials => "Try a command name or a key.",
        Mode::Playground => "Try a scene name.",
        Mode::Paths => "Try another file or folder name.",
        Mode::Screens => "Try a display or window name.",
        Mode::Mirror(_) => "Try a terminal name.",
        Mode::Popup => "Try a command to run.",
    }
}

/// A search that found nothing: what was looked for, quoted, and what the
/// list holds, so the next try is a better one.
fn no_results(query: &str, mode: Mode, g: &Grid, t: &Tokens) -> El {
    let s = &t.space;
    let r = &t.roles;
    col()
        .key("palette-no-results")
        .role("status")
        .w_full()
        .px(s.s6)
        .py(s.s8)
        .gap(s.s2)
        .items_center()
        .child(
            rune::icon(&icons::SEARCH, t.icon.size.xl)
                .color(r.text.faint.paint())
                .a11y_hidden(),
        )
        .child(
            text("No Results")
                .font(g.ty.lead.semibold())
                .color(r.text.text.paint())
                .align(rune::Align::Center),
        )
        .child(
            text(format!(
                "Nothing matches \u{201c}{query}\u{201d}. {}",
                try_hint(mode)
            ))
            .font(g.ty.body)
            .color(r.text.dim.paint())
            .align(rune::Align::Center)
            .wrap()
            .w_full()
            .max_w(EXTENTS.ask_w),
        )
}

/// `at` moved back so `size` fits in `window`, `gap` from its edge. A
/// window not measured yet (zero) leaves it where the press was.
fn fit(at: f32, size: f32, window: f32, gap: f32) -> f32 {
    if window <= 0.0 {
        return at;
    }
    at.min(window - size - gap).max(0.0)
}

#[cfg(test)]
mod tests;
