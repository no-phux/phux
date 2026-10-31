//! The command palette overlay: a scrim, a field, and ranked rows.

use rune::{
    Color, El, PointerPress, Span, TextStyle, Transition, col, el, rich, row, text, text_input,
};

use super::{CAPTION, Ink, LABEL, pressable};
use crate::desk::{CONTEXT_MENU, Desk};
use crate::keys::{self, Shortcut};
use crate::palette::Hit;

const WIDTH: f32 = 600.0;
const MENU_W: f32 = 240.0;
const MENU_ROW: f32 = 28.0;
const MENU_PAD: f32 = 4.0;

pub fn view(desk: &Desk, ink: &Ink) -> El {
    let hits = desk.palette_hits();
    let picked = desk.palette_picked.min(hits.len().saturating_sub(1));
    let prompt = if desk.palette_shortcuts {
        "Filter keyboard shortcuts…"
    } else {
        "Jump to a terminal, session, or action…"
    };
    let field = text_input(&desk.palette_query, TextStyle::new(16.0, 400.0).lh(24.0))
        .key("palette-query")
        .single_line()
        .grow(1.0)
        .color(ink.text)
        .placeholder(prompt, ink.dim)
        .on_edit(|d: &mut Desk| &mut d.palette_query)
        .on_input_key(|d: &mut Desk, event, tx| d.palette_key(event, tx))
        .on_input_change(|d: &mut Desk, _| d.palette_picked = 0)
        .on_submit(|d: &mut Desk, tx| d.palette_run(None, tx));
    let mut results = col().key("palette-results").w_full().padding(6.0).gap(1.0);
    for (index, hit) in hits.iter().take(crate::palette::ROWS).enumerate() {
        results = results.child(hit_row(hit, index, index == picked, ink));
    }
    if hits.is_empty() {
        results = results.child(
            el().w_full()
                .py(18.0)
                .center()
                .child(text("Nothing matches").font(CAPTION).color(ink.dim)),
        );
    }
    let panel = col()
        .key("palette")
        .w(WIDTH)
        .max_w(WIDTH)
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
                .child(
                    text("⌘")
                        .font(TextStyle::new(15.0, 500.0))
                        .color(ink.accent),
                )
                .child(field),
        )
        .child(el().w_full().h(1.0).fill(ink.separator))
        .child(
            col()
                .w_full()
                .max_h(380.0)
                .min_h(0.0)
                .scroll(rune::ScrollSpec {
                    stick_bottom: false,
                })
                .child(results),
        )
        .child(el().w_full().h(1.0).fill(ink.separator))
        .child(
            row()
                .w_full()
                .px(14.0)
                .py(8.0)
                .gap(14.0)
                .child(hint("↑↓", "move", ink))
                .child(hint("↩", "open", ink))
                .child(hint("esc", "close", ink)),
        );
    el().key("palette-scrim")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .fill(Color::BLACK.alpha(0.32))
        .transition(Transition::opacity())
        .modal(true, |d: &mut Desk| d.close_palette())
        .child(col().w_full().pt(96.0).items_center().child(panel))
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
    let height = MENU_ROW * CONTEXT_MENU.len() as f32 + 2.0 * MENU_PAD;
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
    for action in CONTEXT_MENU {
        rows = rows.children(context_row(action, ink));
    }
    el().key("context-scrim")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .on_layout(|d: &mut Desk, rect| d.pointer.window = [rect[2], rect[3]])
        .modal(true, |d: &mut Desk| d.close_menu())
        .child(rows)
}

fn context_row(action: Shortcut, ink: &Ink) -> Option<El> {
    let command = keys::command(action)?;
    Some(
        pressable(format!("context-{action:?}"), ink)
            .w_full()
            .h(MENU_ROW)
            .px(8.0)
            .radius(5.0)
            .items_center()
            .on_press_tx(move |d: &mut Desk, tx| d.context_pick(action, tx))
            .child(text(command.title).font(LABEL).color(ink.text))
            .child(el().grow(1.0))
            .child(text(command.hint()).font(CAPTION).color(ink.dim)),
    )
}

fn hint(keys: &'static str, what: &'static str, ink: &Ink) -> El {
    row()
        .gap(5.0)
        .items_center()
        .child(
            el().px(5.0)
                .radius(4.0)
                .fill(ink.separator)
                .child(text(keys).font(CAPTION).color(ink.text)),
        )
        .child(text(what).font(CAPTION).color(ink.dim))
}

fn kind_mark(kind: &str) -> &'static str {
    match kind {
        "terminal" => "›_",
        "session" => "◫",
        _ => "⚡",
    }
}

fn hit_row(hit: &Hit, index: usize, picked: bool, ink: &Ink) -> El {
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
        .h(36.0)
        .px(12.0)
        .gap(10.0)
        .radius(8.0)
        .items_center()
        .fill(if picked {
            ink.accent.alpha(0.2)
        } else {
            Color::TRANSPARENT
        })
        .on_press_tx(move |d: &mut Desk, tx| d.palette_run(Some(index), tx))
        .child(
            el().w(22.0)
                .center()
                .child(text(kind_mark(hit.item.kind)).font(CAPTION).color(ink.dim)),
        )
        .child(
            rich(title.clone(), spans)
                .font(TextStyle::new(13.5, 450.0))
                .color(ink.text)
                .truncate(),
        )
        .child(el().grow(1.0))
        .child(
            text(hit.item.detail.clone())
                .font(CAPTION)
                .color(ink.dim)
                .truncate(),
        )
}
