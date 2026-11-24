//! The command palette overlay: a scrim, a field, and ranked rows; and the
//! context menu, the kit's.

use crate::ui::tokens::Voice;
use rune::{Cursor, El, PointerPress, Span, col, el, rich, row, text, text_input};
use rune_kit::{MenuItem, context_menu as kit_menu};

use super::overlay::{EXTENTS, Glass, empty_note, glass, key_hint, pop};
use super::tokens::{Paint, Tier, Tokens};
use super::{icons, kit_overlay};
use crate::desk::{CONTEXT_MENU, Desk, TAB_MENU};
use crate::keys::{self, Shortcut};
use crate::palette::{Hit, Mode, Target};
use crate::ui::Cover;

/// `top` is the room above the panel; a short window scrolls its rows.
pub fn view(desk: &Desk, p: &Tokens, top: f32) -> El {
    let t = p;
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
    let query = desk.palette_query.text.trim();
    if hits.is_empty() && phone && search.is_none() && !query.is_empty() {
        results = results.child(no_results(query, t));
    } else if hits.is_empty() {
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
                // A row cut by the footer rule fades rather than slicing
                // mid-glyph, and says more lies below.
                .scroll_edge_fade(t.space.s6)
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

/// A phone's search that found nothing: what was looked for, quoted, and
/// what the list holds, so the next try is a better one.
fn no_results(query: &str, t: &Tokens) -> El {
    let s = &t.space;
    let r = &t.roles;
    let ty = super::tokens::phone::ty(t.type_scale);
    col()
        .key("palette-no-results")
        .role("status")
        .w_full()
        .px(s.s6)
        .pt(s.s8)
        .pb(s.s10)
        .gap(s.s1)
        .items_center()
        .child(
            rune::icon(&icons::SEARCH, t.icon.size.xl)
                .color(r.text.faint.paint())
                .a11y_hidden(),
        )
        .child(el().h(s.s2))
        .child(
            text("No Results")
                .font(ty.lead.semibold())
                .color(r.text.text.paint())
                .align(rune::Align::Center),
        )
        .child(
            text(format!(
                "Nothing matches \u{201c}{query}\u{201d}. Try a command, session or terminal name."
            ))
            .font(ty.body)
            .color(r.text.dim.paint())
            .align(rune::Align::Center)
            .wrap()
            .w_full(),
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

/// Which actions belong together in the menu: a separator falls between
/// groups.
const fn group(action: Shortcut) -> u8 {
    match action {
        // A screenshot goes in as Paste does, so it sits with the edits
        // rather than splitting them with rules of its own.
        Shortcut::Copy | Shortcut::Paste | Shortcut::Screenshot | Shortcut::SelectAll => 0,
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
pub fn context_menu(desk: &Desk, at: PointerPress, p: &Tokens) -> El {
    let t = p;
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
    let mut widest: f32 = 0.0;
    for &action in actions {
        let Some((title, hint)) = context_words(action, locked) else {
            continue;
        };
        if last.is_some_and(|g| g != group(action)) {
            menu = menu.separator();
        }
        last = Some(group(action));
        widest = widest.max(context_row_width(title, &hint, t));
        menu = menu.item(context_item(action, title, hint, locked));
    }
    // The kit holds a menu at a fixed minimum and truncates labels to fit,
    // so "Screenshot to Terminal" read "Screenshot to…". Size it to its
    // longest row instead, as a native menu fits its items.
    let menu = menu.min_width(widest);
    let mut layer = Vec::new();
    let anchor = menu.build(&t.kit, &mut layer);
    for overlay in layer {
        kit_overlay(overlay);
    }
    anchor
}

/// What a context row says: its title and its chord.
fn context_words(action: Shortcut, locked: bool) -> Option<(&'static str, String)> {
    let command = keys::command(action)?;
    // The lock row says which way it goes for the tab under it.
    let title = match action {
        Shortcut::LockTab if locked => "Unlock Tab",
        Shortcut::LockTab => "Lock Tab",
        _ => command.title,
    };
    Some((title, command.hint()))
}

/// The width a kit menu row needs to show `title` and `hint` whole: the
/// list's inset, the row's padding, the label, a clear gap and the chord,
/// each in the face the kit sets them in.
fn context_row_width(title: &str, hint: &str, t: &Tokens) -> f32 {
    let ty = &t.kit.ty;
    let (label, _) = rune::text::measure(title, &[], ty.ui_compact, None);
    let chord = if hint.is_empty() {
        0.0
    } else {
        rune::text::measure(hint, &[], ty.small_medium, None).0 + t.space.s6
    };
    // List inset (2 a side), row padding (12 leading, 6 trailing).
    let chrome = 2.0 * 2.0 + 12.0 + 6.0;
    (label + chord + chrome).ceil()
}

fn context_item(action: Shortcut, title: &'static str, hint: String, locked: bool) -> MenuItem {
    let item = MenuItem::new(title)
        .danger(matches!(action, Shortcut::CloseTerminal | Shortcut::CloseTab))
        // A locked tab refuses to close: say so before the press.
        .disabled(action == Shortcut::CloseTab && locked)
        .on_select_tx(move |d: &mut Desk, tx| d.context_pick(action, tx));
    if hint.is_empty() {
        item
    } else {
        item.shortcut(hint)
    }
}

/// A command's own glyph, where one says it at a glance. The rest have
/// none: one generic glyph repeated down every command row says nothing.
fn action_mark(action: Shortcut) -> Option<&'static rune::Icon> {
    Some(match action {
        Shortcut::NewTerminal | Shortcut::NewSession => &icons::PLUS,
        Shortcut::Paste | Shortcut::Copy => &icons::CLIPBOARD,
        Shortcut::Find | Shortcut::FindNext | Shortcut::FindPrevious => &icons::SEARCH,
        Shortcut::SwitchSession | Shortcut::NextSession | Shortcut::PreviousSession => {
            &icons::WINDOW
        }
        Shortcut::SplitRight => &icons::SPLIT_RIGHT,
        Shortcut::SplitDown => &icons::SPLIT_DOWN,
        Shortcut::ZoomIn
        | Shortcut::ZoomOut
        | Shortcut::ZoomReset
        | Shortcut::InterfaceBigger
        | Shortcut::InterfaceSmaller
        | Shortcut::InterfaceReset => &icons::TEXT_SIZE,
        Shortcut::Settings => &icons::SETTINGS,
        Shortcut::Machines => &icons::MACHINES,
        Shortcut::CloseTerminal | Shortcut::CloseTab => &icons::CLOSE,
        Shortcut::ScrollToBottom => &icons::ARROW_DOWN,
        Shortcut::ScrollToTop => &icons::ARROW_UP,
        Shortcut::Inbox => &icons::INBOX,
        Shortcut::ChangeTheme => &icons::PALETTE,
        Shortcut::RenameSession | Shortcut::RenameTab => &icons::PROMPT,
        _ => return None,
    })
}

/// The glyph for a kind of place. An action without its own glyph has
/// none, so its title stands alone in the column the glyphs keep aligned.
fn kind_mark(kind: &str) -> Option<&'static rune::Icon> {
    match kind {
        "terminal" | "pane" => Some(&icons::PROMPT),
        "session" => Some(&icons::WINDOW),
        "path" => Some(&icons::FOLDER),
        "keep-awake" => Some(&icons::AWAKE),
        _ => None,
    }
}

/// What a row is: an icon, for a theme a swatch of its colours, or for an
/// action without its own glyph nothing.
fn mark(hit: &Hit, washed: bool, t: &Tokens) -> Option<El> {
    let size = t.icon.size.md;
    let Target::Theme(id) = hit.item.target else {
        let ink = if washed {
            t.roles.text.text
        } else {
            t.roles.text.dim
        };
        let glyph = match hit.item.target {
            Target::Action(action) => action_mark(action),
            _ => kind_mark(hit.item.kind),
        };
        return glyph.map(|glyph| rune::icon(glyph, size).color(ink.paint()).a11y_hidden());
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
    Some(
        el().w(size)
            .h(size)
            .radius(t.radius.full)
            .fill(fill)
            .border(t.space.s0_5, ring)
            .a11y_hidden(),
    )
}

/// One ranked result (OWN): its kind or swatch, the title with the
/// matched characters in the accent's text ink one weight heavier, a
/// detail, and for a screen its machine. The picked row is the keyboard's
/// cursor.
fn hit_row(hit: &Hit, index: usize, picked: bool, phone: bool, t: &Tokens) -> El {
    let s = &t.space;
    let r = &t.roles;
    let title = &hit.item.title;
    let heavy = t.ty.label.semibold().weight;
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
        // Nested in the palette's `lg` panel by the list's inset.
        .radius(t.nested(t.radius.lg, s.s1))
        .items_center()
        .focusable(true)
        .focus_selected(picked);
    // A tab's terminals sit indented under its heading.
    let base = if hit.item.kind == "pane" {
        base.pl(s.s3 + s.s6)
    } else {
        base
    };
    let base = if picked {
        base.fill(r.wash.current.paint())
    } else {
        base
    };
    let target = hit.item.target.clone();
    t.kit
        .interactive(base)
        .cursor(Cursor::Pointer)
        .on_press_tx(move |d: &mut Desk, tx| d.palette_run_target(target.clone(), tx))
        .child(el().w(s.s6).center().children(mark(hit, picked, t)))
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
        .child(shortcut_or_detail(hit, detail.paint(), t))
}

/// An action's keys as a key cap (`⇧⌘P`), like every shortcut Rune shows;
/// any other row's detail (its session, a pane id) as words.
fn shortcut_or_detail(hit: &Hit, ink: rune::Color, t: &Tokens) -> El {
    if hit.item.kind == "action" && !hit.item.detail.is_empty() {
        return super::keycap(hit.item.detail.clone(), t);
    }
    detail_text(&hit.item.detail, ink, t)
}

/// A row's detail in its voices: the session's name in the ui face,
/// a pane id (`@N`) after it as a machine noun, mono with tabular figures.
fn detail_text(detail: &str, ink: rune::Color, t: &Tokens) -> El {
    let words = |w: String, font| text(w).font(font).color(ink).truncate();
    let id = detail.rsplit_once(" · ").filter(|(_, id)| {
        id.strip_prefix('@')
            .is_some_and(|n| n.parse::<u32>().is_ok())
    });
    match id {
        Some((name, id)) => row()
            .items_center()
            .min_w(0.0)
            .child(
                words(format!("{name} · "), t.ty.caption)
                    .shrink()
                    .min_w(0.0),
            )
            .child(words(id.to_owned(), t.ty.readout))
            .a11y_hidden(),
        None => words(detail.to_owned(), t.ty.caption).a11y_hidden(),
    }
}

#[cfg(test)]
mod tests;
