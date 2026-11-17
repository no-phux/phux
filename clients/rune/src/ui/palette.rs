//! The command palette overlay: a scrim, a field, and ranked rows; and the
//! context menu, the kit's.

use rune::{Cursor, El, PointerPress, Span, col, el, rich, row, text, text_input};
use rune_kit::{MenuItem, context_menu as kit_menu};

use super::overlay::{EXTENTS, Glass, empty_note, glass, key_hint, pop, strong};
use super::tokens::{Paint, Tier, Tokens};
use super::{icons, kit_overlay};
use crate::desk::{CONTEXT_MENU, Desk, TAB_MENU};
use crate::keys::{self, Shortcut};
use crate::palette::{Hit, Mode, Target};
use crate::ui::Cover;

/// `top` is the room above the panel; a short window scrolls its rows.
pub fn view(desk: &Desk, p: &impl AsRef<Tokens>, top: f32) -> El {
    let t = p.as_ref();
    let s = &t.space;
    let r = &t.roles;
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
        Mode::Playground => "Open a playground scene…",
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
    let field = text_input(&desk.palette_query, t.ty.lead)
        .key("palette-query")
        .label("Command search")
        .single_line()
        .grow(1.0)
        .color(r.text.text.paint())
        .placeholder(prompt, r.text.faint.paint())
        .on_edit(|d: &mut Desk| &mut d.palette_query)
        .on_input_key(|d: &mut Desk, event, tx| d.palette_key(event, tx))
        .on_input_change(|d: &mut Desk, _| d.palette_edited())
        .on_submit(|d: &mut Desk, tx| d.palette_run(None, tx));
    // One list the arrows walk, its picked row the keyboard's cursor.
    let mut results = col()
        .key("palette-results")
        .role("listbox")
        .label("Results")
        .focus_group()
        .w_full()
        .padding(s.s1);
    for (index, hit) in hits.iter().take(crate::palette::ROWS).enumerate() {
        results = results.child(hit_row(hit, index, index == picked, phone, t));
    }
    let search = desk.connection.paths.as_ref();
    let empty = match search {
        Some(s) if s.searching() => "Searching…",
        Some(s) if s.note.is_some() && hits.is_empty() => s.note.as_deref().unwrap_or_default(),
        _ if mode == Mode::Screens && hits.is_empty() => {
            desk.connection.resources.screens.empty_text()
        }
        _ => "Nothing matches",
    };
    if hits.is_empty() {
        results = results.child(empty_note(empty, t));
    } else if let Some(note) = search.and_then(|s| s.note.clone()) {
        results = results.child(
            el().w_full()
                .px(s.s3)
                .py(s.s1)
                .child(text(note).font(t.ty.caption).color(r.text.dim.paint())),
        );
    }
    let (width, rows_h) = if mode == Mode::Paths {
        (EXTENTS.paths_w, EXTENTS.paths_rows_h)
    } else {
        (EXTENTS.palette_w, EXTENTS.palette_rows_h)
    };
    let rule = |key: &'static str| {
        rune::rule(s.hairline, r.border.base.paint())
            .w_full()
            .key(key)
    };
    let panel = col()
        .key("palette")
        // A narrow window (the phone form) takes what it has.
        .w_full()
        .max_w(width)
        .shrink()
        .min_h(0.0)
        .radius(t.radius.lg)
        .clip_rounded(t.radius.lg)
        .border(s.hairline, r.border.base.paint())
        .transition(pop(t))
        .child(
            row()
                .w_full()
                .h(s.s12)
                .px(s.s4)
                .gap(s.s3)
                .items_center()
                .child(
                    rune::icon(glyph, t.icon.size.md)
                        .color(r.accent.accent_text.paint())
                        .a11y_hidden(),
                )
                .child(field),
        )
        .child(rule("palette-header-rule"))
        .child(
            col()
                .w_full()
                .max_h(rows_h)
                .shrink()
                .min_h(0.0)
                .scroll(rune::ScrollSpec {
                    stick_bottom: false,
                })
                .child(results),
        )
        // A phone taps its rows; the keys this footer teaches are a desktop's.
        .children((!phone).then(|| rule("palette-footer-rule")))
        .children((!phone).then(|| {
            row()
                .w_full()
                .px(s.s3)
                .py(s.s2)
                .gap(s.s3)
                .child(key_hint("↑↓", "move", t))
                .child(key_hint("↩", open, t))
                .child(key_hint("esc", "close", t))
        }));
    let panel = t.elevate(
        glass(panel, t, r.layer.overlay, Glass::Overlay, phone),
        Tier::Overlay,
    );
    let scrim = el()
        .key("palette-scrim")
        .cover()
        .on_layout(|d: &mut Desk, rect| d.pointer.window = [rect[2], rect[3]])
        .transition(rune::Transition::opacity())
        .modal(true, |d: &mut Desk| d.close_palette());
    // Paths open where they'll land.
    // A phone has no room beside the cursor: its paths take the sheet.
    let at_cursor = (mode == Mode::Paths && !phone)
        .then(|| desk.cursor_point())
        .flatten();
    match at_cursor {
        Some([x, y]) => {
            let [window_w, window_h] = desk.pointer.window;
            let gap = s.s1;
            let below = y + gap;
            let tall = EXTENTS.paths_h;
            // No room under the line: sit above it instead.
            let y = if window_h > 0.0 && below + tall > window_h {
                (y - desk.metrics().cell_h - tall - gap).max(0.0)
            } else {
                below
            };
            scrim.child(panel.absolute(fit(x, EXTENTS.paths_w, window_w, gap), y))
        }
        None => scrim.fill(r.layer.scrim.paint()).child(
            col()
                .w_full()
                .h_full()
                .pt(top)
                .px(s.s3)
                .pb(s.s3)
                .items_center()
                .child(panel),
        ),
    }
}

/// `at` moved back so `size` fits in `window`, `gap` from its edge. A
/// window not measured yet (zero) leaves it where the press was.
fn fit(at: f32, size: f32, window: f32, gap: f32) -> f32 {
    if window <= 0.0 {
        return at;
    }
    at.min(window - size - gap).max(0.0)
}

/// Which actions belong together in the menu: a separator falls between
/// groups.
const fn group(action: Shortcut) -> u8 {
    match action {
        Shortcut::Copy | Shortcut::Paste | Shortcut::SelectAll => 0,
        Shortcut::Find => 1,
        Shortcut::SplitRight | Shortcut::SplitDown => 2,
        Shortcut::LockTab | Shortcut::RenameTab => 3,
        _ => 4,
    }
}

/// The right-click menu over a pane or a tab (kit `context_menu`): it opens
/// at the press, flips to stay inside the window, takes the keyboard
/// (arrows, Home and End, Return, Escape) and gives focus back on close.
/// The menu mounts in the kit layer; what this returns marks its place.
pub fn context_menu(desk: &Desk, at: PointerPress, p: &impl AsRef<Tokens>) -> El {
    let t = p.as_ref();
    let actions: &[Shortcut] = if desk.pointer.tab_menu {
        &TAB_MENU
    } else {
        &CONTEXT_MENU
    };
    let locked = desk
        .connection
        .tabs
        .list
        .iter()
        .any(|tab| tab.active && tab.locked);
    let mut menu = kit_menu("context", el(), Some(at)).on_close(|d: &mut Desk| d.close_menu());
    let mut last = None;
    for &action in actions {
        let Some(item) = context_item(action, locked) else {
            continue;
        };
        if last.is_some_and(|g| g != group(action)) {
            menu = menu.separator();
        }
        last = Some(group(action));
        menu = menu.item(item);
    }
    let mut layer = Vec::new();
    let anchor = menu.build(&t.kit, &mut layer);
    for overlay in layer {
        kit_overlay(overlay);
    }
    anchor
}

fn context_item(action: Shortcut, locked: bool) -> Option<MenuItem> {
    let command = keys::command(action)?;
    // The lock row says which way it goes for the tab under it.
    let title = match action {
        Shortcut::LockTab if locked => "Unlock Tab",
        Shortcut::LockTab => "Lock Tab",
        _ => command.title,
    };
    let hint = command.hint();
    let item = MenuItem::new(title)
        .danger(matches!(action, Shortcut::CloseTerminal | Shortcut::CloseTab))
        // A locked tab refuses to close: say so before the press.
        .disabled(action == Shortcut::CloseTab && locked)
        .on_select_tx(move |d: &mut Desk, tx| d.context_pick(action, tx));
    Some(if hint.is_empty() {
        item
    } else {
        item.shortcut(hint)
    })
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
fn mark(hit: &Hit, washed: bool, t: &Tokens) -> El {
    let size = t.icon.size.md;
    let Target::Theme(id) = hit.item.target else {
        let ink = if washed {
            t.roles.text.text
        } else {
            t.roles.text.dim
        };
        return rune::icon(kind_mark(hit.item.kind), size)
            .color(ink.paint())
            .a11y_hidden();
    };
    let theme = id.and_then(crate::theme::find);
    let (fill, ring) = theme.map_or(
        (t.roles.layer.ground.paint(), t.roles.border.control.paint()),
        |t| {
            (
                crate::theme::color(t.background),
                crate::theme::color(t.accent),
            )
        },
    );
    el().w(size)
        .h(size)
        .radius(t.radius.full)
        .fill(fill)
        .border(t.space.s0_5, ring)
        .a11y_hidden()
}

/// One ranked result (§4.1.3 OWN): its kind or swatch, the title with the
/// matched characters in the accent's text ink one weight heavier, a
/// detail, and for a screen its machine. The picked row is the keyboard's
/// cursor.
fn hit_row(hit: &Hit, index: usize, picked: bool, phone: bool, t: &Tokens) -> El {
    let s = &t.space;
    let r = &t.roles;
    let title = &hit.item.title;
    let heavy = strong(t.ty.label).weight;
    let spans = hit
        .matched
        .iter()
        .filter_map(|&at| title.char_indices().nth(at))
        .map(|(start, ch)| {
            Span::new(start, start + ch.len_utf8())
                .color(r.accent.accent_text.paint())
                .weight(heavy)
        })
        .collect();
    // Faint never sits on a wash: the picked row's detail reads dim.
    let detail = if picked { r.text.dim } else { r.text.faint };
    let row_h = if phone { s.row_h_touch } else { s.row_h };
    let base = row()
        .key(format!("palette-row-{index}"))
        .role("option")
        .label(title.clone())
        .description(hit.item.detail.clone())
        .w_full()
        .min_h(row_h)
        .py(s.s1)
        .px(s.s3)
        .gap(s.s3)
        .radius(t.radius.sm)
        .items_center()
        .focusable(true)
        .focus_selected(picked);
    let base = if picked {
        base.fill(r.wash.current.paint())
    } else {
        base
    };
    t.kit
        .interactive(base)
        .cursor(Cursor::Pointer)
        .on_press_tx(move |d: &mut Desk, tx| d.palette_run(Some(index), tx))
        .child(el().w(s.s6).center().child(mark(hit, picked, t)))
        .child(
            rich(title.clone(), spans)
                .font(t.ty.label)
                .color(r.text.text.paint())
                .truncate()
                .min_w(0.0)
                .shrink()
                .a11y_hidden(),
        )
        .child(el().grow(1.0))
        .children(match &hit.item.target {
            Target::Screen(host, _) => {
                let host = host.as_deref().unwrap_or(crate::machines::LOCAL);
                Some(crate::host_color::dot(host, s.s2, r.layer.overlay.paint()))
            }
            _ => None,
        })
        .child(
            text(hit.item.detail.clone())
                .font(t.ty.caption)
                .color(detail.paint())
                .truncate()
                .a11y_hidden(),
        )
}

#[cfg(test)]
mod tests;
