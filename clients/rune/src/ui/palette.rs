//! The command palette overlay: a scrim, a field, and ranked rows.

use rune::{
    Color, El, PointerPress, Span, TextStyle, Transition, col, el, rich, row, text, text_input,
};

use super::{CAPTION, Ink, LABEL, icons, pressable};
use crate::desk::{CONTEXT_MENU, Desk, TAB_MENU};
use crate::keys::{self, Shortcut};
use crate::palette::{Hit, Mode, Target};

const WIDTH: f32 = 600.0;
/// The path picker, narrower, at the cursor.
const PATHS_WIDTH: f32 = 460.0;
const PATHS_HEIGHT: f32 = 340.0;
const MENU_W: f32 = 240.0;
const MENU_ROW: f32 = 28.0;
const MENU_PAD: f32 = 4.0;

/// `top` is the room above the panel; a short window scrolls its rows.
pub fn view(desk: &Desk, ink: &Ink, top: f32) -> El {
    let hits = desk.palette_hits();
    let picked = desk.palette_picked.min(hits.len().saturating_sub(1));
    let mode = desk.palette_mode;
    let phone = desk.phone.is_some();
    let prompt = match mode {
        Mode::All if phone => "Search commands and terminals…",
        Mode::All => "Jump to a terminal, session, or action…",
        Mode::Mirror(source) => source.prompt(),
        Mode::Shortcuts => "Filter keyboard shortcuts…",
        Mode::Essentials => "The keys worth learning first; pick one to try it…",
        Mode::Sessions => "Switch session, or name a new one…",
        Mode::Themes => "Preview a theme…",
        Mode::Paths => "Find a file or folder…",
        Mode::Screens => "Filter displays and windows…",
        Mode::Popup => "A command to run in a popup…",
    };
    let (open, glyph) = match mode {
        Mode::Paths => ("insert", &icons::FOLDER),
        Mode::Themes => ("keep", &icons::PALETTE),
        Mode::Sessions => ("switch", &icons::WINDOW),
        _ => ("open", &icons::SEARCH),
    };
    let field = text_input(&desk.palette_query, TextStyle::new(16.0, 400.0).lh(24.0))
        .key("palette-query")
        .single_line()
        .grow(1.0)
        .color(ink.text)
        .placeholder(prompt, ink.dim)
        .on_edit(|d: &mut Desk| &mut d.palette_query)
        .on_input_key(|d: &mut Desk, event, tx| d.palette_key(event, tx))
        .on_input_change(|d: &mut Desk, _| d.palette_edited())
        .on_submit(|d: &mut Desk, tx| d.palette_run(None, tx));
    let mut results = col().key("palette-results").w_full().padding(6.0).gap(1.0);
    for (index, hit) in hits.iter().take(crate::palette::ROWS).enumerate() {
        results = results.child(hit_row(hit, index, index == picked, phone, ink));
    }
    let search = desk.paths.as_ref();
    let empty = match search {
        Some(s) if s.searching() => "Searching…",
        Some(s) if s.note.is_some() && hits.is_empty() => s.note.as_deref().unwrap_or_default(),
        _ if mode == Mode::Screens && hits.is_empty() => desk.resources.screens.empty_text(),
        _ => "Nothing matches",
    };
    if hits.is_empty() {
        results = results.child(
            el().w_full()
                .py(18.0)
                .center()
                .child(text(empty.to_owned()).font(CAPTION).color(ink.dim)),
        );
    } else if let Some(note) = search.and_then(|s| s.note.clone()) {
        results = results.child(
            el().w_full()
                .px(12.0)
                .py(6.0)
                .child(text(note).font(CAPTION).color(ink.dim)),
        );
    }
    let width = if mode == Mode::Paths {
        PATHS_WIDTH
    } else {
        WIDTH
    };
    let panel = col()
        .key("palette")
        // A narrow window (the phone form) takes what it has.
        .w_full()
        .max_w(width)
        .shrink()
        .min_h(0.0)
        .radius(14.0)
        .clip_rounded(14.0)
        .fill(ink.raised)
        .border(1.0, Color::WHITE.alpha(0.08))
        .shadow(18.0, 56.0, Color::BLACK.alpha(0.5))
        .transition(Transition::scale(0.97).combined(Transition::opacity()))
        .child(
            row()
                .w_full()
                .h(54.0)
                .px(18.0)
                .gap(12.0)
                .items_center()
                .child(rune::icon(glyph, 16.0).color(ink.accent).a11y_hidden())
                .child(field),
        )
        .child(rune::rule(1.0, ink.separator).w_full().key("palette-header-rule"))
        .child(
            col()
                .w_full()
                .max_h(if mode == Mode::Paths { 240.0 } else { 380.0 })
                .shrink()
                .min_h(0.0)
                .scroll(rune::ScrollSpec {
                    stick_bottom: false,
                })
                .child(results),
        )
        // A phone taps its rows; the keys this footer teaches are a desktop's.
        .children((!phone).then(|| {
            rune::rule(1.0, ink.separator).w_full().key("palette-footer-rule")
        }))
        .children((!phone).then(|| {
            row()
                .w_full()
                .px(14.0)
                .py(8.0)
                .gap(14.0)
                .child(hint("↑↓", "move", ink))
                .child(hint("↩", open, ink))
                .child(hint("esc", "close", ink))
        }));
    let scrim = el()
        .key("palette-scrim")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .on_layout(|d: &mut Desk, rect| d.pointer.window = [rect[2], rect[3]])
        .transition(Transition::opacity())
        .modal(true, |d: &mut Desk| d.close_palette());
    // Paths open where they'll land.
    // A phone has no room beside the cursor: its paths take the sheet.
    let at_cursor = (mode == Mode::Paths && desk.phone.is_none())
        .then(|| desk.cursor_point())
        .flatten();
    match at_cursor {
        Some([x, y]) => {
            let [window_w, window_h] = desk.pointer.window;
            let below = y + 4.0;
            // No room under the line: sit above it instead.
            let y = if window_h > 0.0 && below + PATHS_HEIGHT > window_h {
                (y - desk.metrics().cell_h - PATHS_HEIGHT - 4.0).max(0.0)
            } else {
                below
            };
            scrim.child(panel.absolute(fit(x, PATHS_WIDTH, window_w), y))
        }
        None => scrim.fill(Color::BLACK.alpha(0.32)).child(
            col()
                .w_full()
                .h_full()
                .pt(top)
                .px(12.0)
                .pb(12.0)
                .items_center()
                .child(panel),
        ),
    }
}

/// `at` moved back so `size` fits in `window`. A window not measured yet
/// (zero) leaves it where the press was.
fn fit(at: f32, size: f32, window: f32) -> f32 {
    if window <= 0.0 {
        return at;
    }
    at.min(window - size - 4.0).max(0.0)
}

/// The right-click menu over a pane, kept inside the window.
pub fn context_menu(desk: &Desk, at: PointerPress, ink: &Ink) -> El {
    let actions: &[Shortcut] = if desk.pointer.tab_menu {
        &TAB_MENU
    } else {
        &CONTEXT_MENU
    };
    let height = MENU_ROW * actions.len() as f32 + 2.0 * MENU_PAD;
    let [window_w, window_h] = desk.pointer.window;
    let mut rows = col()
        .key("context-menu")
        .absolute(fit(at.x, MENU_W, window_w), fit(at.y, height, window_h))
        .w(MENU_W)
        .padding(MENU_PAD);
    rows = rows
        .radius(8.0)
        .fill(ink.raised)
        .border(1.0, ink.separator)
        .shadow(8.0, 24.0, Color::BLACK.alpha(0.4));
    let locked = desk.tabs.list.iter().any(|tab| tab.active && tab.locked);
    for &action in actions {
        rows = rows.children(context_row(action, locked, ink));
    }
    el().key("context-scrim")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .on_layout(|d: &mut Desk, rect| d.pointer.window = [rect[2], rect[3]])
        .modal(true, |d: &mut Desk| d.close_menu())
        .child(rows)
}

fn context_row(action: Shortcut, locked: bool, ink: &Ink) -> Option<El> {
    let command = keys::command(action)?;
    // The lock row says which way it goes for the tab under it.
    let title = match action {
        Shortcut::LockTab if locked => "Unlock Tab",
        Shortcut::LockTab => "Lock Tab",
        _ => command.title,
    };
    Some(
        pressable(format!("context-{action:?}"), ink)
            .w_full()
            .h(MENU_ROW)
            .px(8.0)
            .radius(5.0)
            .items_center()
            .on_press_tx(move |d: &mut Desk, tx| d.context_pick(action, tx))
            .child(text(title).font(LABEL).color(ink.text))
            .child(el().grow(1.0))
            .child(text(command.hint()).font(CAPTION).color(ink.dim)),
    )
}

fn hint(keys: &'static str, what: &'static str, ink: &Ink) -> El {
    row()
        .gap(5.0)
        .items_center()
        .child(super::keycap(keys, ink))
        .child(text(what).font(CAPTION).color(ink.dim))
}

fn kind_mark(kind: &str) -> &'static rune::Icon {
    match kind {
        "terminal" => &icons::PROMPT,
        "session" => &icons::WINDOW,
        "path" => &icons::FOLDER,
        _ => &icons::BOLT,
    }
}

/// What a row is: an icon, or for a theme a swatch of its colours.
fn mark(hit: &Hit, ink: &Ink) -> El {
    let Target::Theme(id) = hit.item.target else {
        return rune::icon(kind_mark(hit.item.kind), 14.0)
            .color(ink.dim)
            .a11y_hidden();
    };
    let theme = id.and_then(crate::theme::find);
    let (fill, ring) = theme.map_or((ink.bg, ink.separator), |t| {
        (
            crate::theme::color(t.background),
            crate::theme::color(t.accent),
        )
    });
    el().size(14.0, 14.0)
        .radius(7.0)
        .fill(fill)
        .border(2.0, ring)
}

fn hit_row(hit: &Hit, index: usize, picked: bool, phone: bool, ink: &Ink) -> El {
    let title = &hit.item.title;
    // Matched characters in the accent, at their byte ranges.
    let spans = hit
        .matched
        .iter()
        .filter_map(|&at| title.char_indices().nth(at))
        .map(|(start, ch)| {
            Span::new(start, start + ch.len_utf8())
                .color(ink.accent)
                .weight(650.0)
        })
        .collect();
    pressable(format!("palette-row-{index}"), ink)
        .w_full()
        .h(if phone { 44.0 } else { 36.0 })
        .px(12.0)
        .gap(10.0)
        .radius(8.0)
        .items_center()
        .fill(if picked {
            ink.current
        } else {
            Color::TRANSPARENT
        })
        .on_press_tx(move |d: &mut Desk, tx| d.palette_run(Some(index), tx))
        .child(el().w(22.0).center().child(mark(hit, ink)))
        .child(
            rich(title.clone(), spans)
                .font(TextStyle::new(13.5, 450.0))
                .color(ink.text)
                .truncate(),
        )
        .child(el().grow(1.0))
        .children(match &hit.item.target {
            Target::Screen(host, _) => {
                let host = host.as_deref().unwrap_or(crate::machines::LOCAL);
                Some(crate::host_color::dot(host, 7.0, ink.raised))
            }
            _ => None,
        })
        .child(
            text(hit.item.detail.clone())
                .font(CAPTION)
                .color(ink.dim)
                .truncate(),
        )
}
