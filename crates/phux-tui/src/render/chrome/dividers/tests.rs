use super::*;

use std::collections::HashMap;

use ratatui::style::Color;

use crate::attach::multi_pane::{DividerCell, compute_layout_in};
use crate::layout::{LayoutNode, LayoutState, SplitDir, split_at};
use crate::render::chrome::agent_badge;
mod titles;

fn t(id: u32) -> ResourceId {
    ResourceId::local(id)
}

fn leaf(id: u32) -> LayoutNode {
    LayoutNode::Leaf(t(id))
}

/// A pane area with one rail row above it, the shipped shape.
const fn railed(cols: u16, rows: u16) -> Rect {
    Rect {
        x: 0,
        y: 1,
        w: cols,
        h: rows - 1,
    }
}

fn theme() -> Theme {
    Theme::default()
}

/// The rail row: the one above the pane area, if any.
fn rail_row(content: Rect) -> Option<u16> {
    content.y.checked_sub(1)
}

fn label(text: &'static str) -> PaneLabel<'static> {
    PaneLabel {
        text,
        agent: None,
        attention: false,
        seen: true,
        host: None,
        unreachable: false,
        origin: None,
        scrolled_back: None,
        exit: None,
        slot_tab: None,
    }
}

/// Render with every knob explicit.
fn render_full(
    layout: &PaneLayout,
    content: Rect,
    rail: Option<u16>,
    focus: Option<&ResourceId>,
    pane_label: Option<PaneLabel<'static>>,
) -> String {
    let mut bytes: Vec<u8> = Vec::new();
    render_dividers(&mut bytes, layout, content, rail, focus, &theme(), |_| {
        pane_label
    })
    .unwrap();
    String::from_utf8(bytes).unwrap()
}

/// Render focused on pane 1 with `pane_label` on every pane.
fn render_label(
    layout: &PaneLayout,
    content: Rect,
    pane_label: Option<PaneLabel<'static>>,
) -> String {
    render_full(layout, content, rail_row(content), Some(&t(1)), pane_label)
}

/// Render with a plain text label on every pane.
fn render(layout: &PaneLayout, content: Rect, text: Option<&'static str>) -> String {
    render_label(layout, content, text.map(label))
}

fn state(tree: LayoutNode, focus: u32) -> LayoutState {
    LayoutState {
        tree: Some(tree),
        focus: Some(t(focus)),
    }
}

fn layout_of(state: &LayoutState, content: Rect) -> PaneLayout {
    compute_layout_in(state, content, (content.w, content.y + content.h))
}

/// `1 | 2`, focused on 1.
fn split_layout(content: Rect) -> PaneLayout {
    let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
    layout_of(&state(tree, 1), content)
}

/// `(1 / 3) | 2`, focused on `focus`.
fn cross_layout(content: Rect, focus: u32) -> PaneLayout {
    let t1 = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
    let t2 = split_at(&t1, &t(1), &t(3), SplitDir::Vertical, 0.5).unwrap();
    layout_of(&state(t2, focus), content)
}

fn lone_layout(content: Rect) -> PaneLayout {
    layout_of(&state(leaf(1), 1), content)
}

fn rect_contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && y >= r.y && x < r.x.saturating_add(r.w) && y < r.y.saturating_add(r.h)
}

/// No painted cell lands in a pane interior or past the area's right
/// edge (a wide glyph that walked past it would wrap into a pane row).
fn assert_chrome_only(layout: &PaneLayout, content: Rect, s: &str) {
    let right_edge = content.x.saturating_add(content.w);
    for (x, y, sym) in painted_cells(s) {
        assert!(x < right_edge, "{sym:?} at column {x}, past {right_edge}");
        for r in layout.rects.values() {
            assert!(
                !rect_contains(*r, x, y),
                "painted {sym:?} at ({x}, {y}) inside {r:?}"
            );
        }
    }
}

/// The highest column painted on the rail row.
fn rail_extent(s: &str, content: Rect) -> u16 {
    let rail_y = rail_row(content).unwrap();
    painted_cells(s)
        .into_iter()
        .filter(|(_, y, _)| *y == rail_y)
        .map(|(x, _, _)| x)
        .max()
        .unwrap()
}

#[test]
fn empty_or_zero_width_layouts_paint_nothing() {
    let empty = PaneLayout {
        viewport: (80, 24),
        rects: HashMap::new(),
        dividers: Vec::new(),
        divider_hits: Vec::new(),
    };
    let full = Rect {
        x: 0,
        y: 0,
        w: 80,
        h: 24,
    };
    assert!(render(&empty, full, None).is_empty());
    let zero = PaneLayout {
        viewport: (0, 24),
        dividers: vec![DividerCell {
            x: 0,
            y: 0,
            ch: '\u{2502}',
        }],
        ..empty
    };
    assert!(render(&zero, Rect { w: 0, ..full }, None).is_empty());
}

/// THE load-bearing skip-cell invariant: libghostty owns pane interiors
/// (ADR-0020), so no chrome byte may land there, for a split, a cross
/// split, or a wide title.
#[test]
fn chrome_never_paints_inside_a_pane() {
    let content = railed(80, 24);
    for layout in [split_layout(content), cross_layout(content, 2)] {
        let s = render(&layout, content, Some("shell"));
        assert!(!extract_cups(&s).is_empty());
        assert_chrome_only(&layout, content, &s);
        assert!(
            s.starts_with("\x1b[0m") && s.ends_with("\x1b[0m"),
            "SGR resets"
        );
    }
    let narrow = railed(40, 10);
    let layout = split_layout(narrow);
    let s = render(&layout, narrow, Some("日本語のペイン名前"));
    assert_chrome_only(&layout, narrow, &s);
    let rail_y = rail_row(narrow).unwrap();
    assert!(
        painted_cells(&s)
            .iter()
            .all(|(_, y, _)| *y == rail_y || layout.dividers.iter().any(|d| d.y == *y))
    );
}

/// Structural twin of the above: compose marks every interior cell skip
/// and every divider cell with its glyph.
#[test]
fn compose_buffer_marks_pane_interiors_skip() {
    let content = railed(80, 24);
    let layout = split_layout(content);
    let buf = compose_buffer(
        &layout,
        content,
        rail_row(content),
        Some(&t(1)),
        &theme(),
        |_| None,
    );
    for r in layout.rects.values() {
        for y in r.y..r.y + r.h {
            for x in r.x..r.x + r.w {
                assert!(
                    buf.cell((x, y)).unwrap().diff_option == CellDiffOption::Skip,
                    "({x}, {y})"
                );
            }
        }
    }
    for d in &layout.dividers {
        let cell = buf.cell((d.x, d.y)).unwrap();
        assert!(
            cell.diff_option != CellDiffOption::Skip,
            "({}, {})",
            d.x,
            d.y
        );
        assert_eq!(cell.symbol().chars().next(), Some(d.ch));
    }
}

/// Focus is a colour, never a heavier stroke (mixed-weight junctions do
/// not render); unfocused rules recede, and a lone pane is not tinted.
#[test]
fn focus_tints_rules_and_nothing_else() {
    let content = railed(80, 24);
    let s = render(&split_layout(content), content, None);
    assert!(!s.contains('\u{2503}') && !s.contains('\u{2501}') && s.contains('\u{2502}'));
    assert!(s.contains(&sgr_fg(theme().divider_focus)), "{s:?}");
    assert!(!s.contains("\x1b[1m"), "focus is colour, not bold");

    let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
    let tree = split_at(&tree, &t(2), &t(3), SplitDir::Horizontal, 0.5).unwrap();
    let s = render(&layout_of(&state(tree, 1), content), content, None);
    assert!(s.contains(&sgr_fg(theme().divider)), "{s:?}");

    let s = render(&lone_layout(content), content, None);
    assert!(!s.contains(&sgr_fg(theme().divider_focus)) && s.contains(&sgr_fg(theme().divider)));
}

/// Rules and titles sit on the terminal's own background only when the
/// roles were solved against it; otherwise every chrome cell carries the
/// surface fill (spec §4.2).
#[test]
fn an_unknown_ground_fills_rules_and_titles_with_the_surface() {
    let content = railed(80, 24);
    let layout = split_layout(content);
    let paint = |theme: &Theme| {
        let mut bytes: Vec<u8> = Vec::new();
        render_dividers(
            &mut bytes,
            &layout,
            content,
            rail_row(content),
            Some(&t(1)),
            theme,
            |_| Some(label("build")),
        )
        .unwrap();
        String::from_utf8(bytes).unwrap()
    };
    let unknown = theme();
    let mut fill = Vec::new();
    crate::render::sgr::write_sgr_color(&mut fill, unknown.surface, false).unwrap();
    let fill = String::from_utf8(fill).unwrap();
    let s = paint(&unknown);
    assert!(s.contains(&fill), "{s:?}");
    let known = Theme::house(&crate::render::theme::Outer {
        background: Some(phux_design::Rgb8::hex(0x14_1619)),
        ..crate::render::theme::Outer::default()
    });
    assert!(!paint(&known).contains("\x1b[48;"), "known ground: no fill");
}

/// A retained pane that exited wears its ladder mark on the title: the
/// failed cross for a nonzero status, the ended dash for a clean one.
#[test]
fn an_exited_pane_wears_failed_or_ended() {
    let th = theme();
    let with_exit = |code| PaneLabel {
        exit: Some(code),
        ..label("build")
    };
    let failed = with_exit(2).badge(&th).expect("badge");
    assert_eq!((failed.glyph, failed.color), ("✕", th.agent_failed));
    assert!(failed.emphatic);
    let ended = with_exit(0).badge(&th).expect("badge");
    assert_eq!(ended.glyph, "–");
    assert!(
        label("build").badge(&th).is_none(),
        "a live shell wears none"
    );
}

/// Emphasis follows the client's focused pane, not the layout tree's
/// remembered focus (the two diverge until the layout is persisted).
#[test]
fn emphasis_follows_the_passed_focus_not_the_layout_tree() {
    let content = railed(80, 24);
    let layout = split_layout(content);
    let one = layout.rects[&t(1)];
    let two = layout.rects[&t(2)];
    let rail_y = rail_row(content).unwrap();
    for (focus, on, off) in [(2, two, one), (1, one, two)] {
        let cells = styled_cells(&render_full(
            &layout,
            content,
            Some(rail_y),
            Some(&t(focus)),
            None,
        ));
        assert!(
            cells[&(on.x + 2, rail_y)].1 && !cells[&(off.x + 2, rail_y)].1,
            "focus {focus}"
        );
    }
}

/// The rail tees into each vertical rule; without a reserved row (or
/// with a top bar in the row above) no rail is painted at all.
#[test]
fn the_rail_tees_and_is_never_invented() {
    let content = railed(80, 24);
    let layout = split_layout(content);
    let cells: HashMap<(u16, u16), String> = painted_cells(&render(&layout, content, None))
        .into_iter()
        .map(|(x, y, s)| ((x, y), s))
        .collect();
    let col = layout.dividers.first().unwrap().x;
    assert_eq!(cells.get(&(col, 0)).map(String::as_str), Some("┬"));
    assert_eq!(cells.get(&(0, 0)).map(String::as_str), Some("─"));
    assert_eq!(cells.get(&(79, 0)).map(String::as_str), Some("─"));

    // A top-docked bar also puts content at row 1; with `rail: None` the
    // bar's row must stay untouched.
    let content = Rect {
        x: 0,
        y: 1,
        w: 40,
        h: 1,
    };
    let s = render_full(
        &lone_layout(content),
        content,
        None,
        Some(&t(1)),
        Some(label("shell")),
    );
    assert!(painted_cells(&s).iter().all(|(_, y, _)| *y != 0), "{s:?}");
}

/// Each painted cell as `(symbol, accented)` (accented = focus style).
fn styled_cells(s: &str) -> HashMap<(u16, u16), (String, bool)> {
    let focus_sgr = sgr_fg(theme().divider_focus);
    let mut out = HashMap::new();
    let (mut x, mut y) = (0u16, 0u16);
    let mut accented = false;
    let mut rest = s;
    let mut put = |text: &str, x: &mut u16, y: u16, accented: bool| {
        for c in text.chars() {
            let w = u16::try_from(crate::render::cell_width(c).unwrap_or(0)).unwrap_or(1);
            if w == 0 {
                continue;
            }
            out.insert((*x, y), (c.to_string(), accented));
            *x = x.saturating_add(w);
        }
    };
    while let Some(i) = rest.find('\x1b') {
        put(&rest[..i], &mut x, y, accented);
        let tail = &rest[i..];
        if tail.starts_with(&focus_sgr) {
            accented = true;
            rest = &tail[focus_sgr.len()..];
            continue;
        }
        let end = tail[1..]
            .find(|c: char| c.is_ascii_alphabetic())
            .map_or(tail.len(), |j| j + 2);
        let seq = &tail[..end];
        if seq.ends_with('H')
            && let Some((rr, cc)) = seq[2..seq.len() - 1].split_once(';')
            && let (Ok(rn), Ok(cn)) = (rr.parse::<u16>(), cc.parse::<u16>())
        {
            y = rn.saturating_sub(1);
            x = cn.saturating_sub(1);
        } else if seq == "\x1b[0m" {
            // Every run opens with a full reset; a ground fill after the
            // focus colour does not end the emphasis.
            accented = false;
        }
        rest = &tail[end..];
    }
    put(rest, &mut x, y, accented);
    out
}

fn sgr_fg(color: Color) -> String {
    let mut out: Vec<u8> = Vec::new();
    crate::render::sgr::write_sgr_color(&mut out, color, true).unwrap();
    String::from_utf8(out).unwrap()
}

/// Decode the stream into `(x, y, symbol)` per painted cell, advancing by
/// display width exactly as a terminal does (a per-char harness would
/// share the very bug it exists to catch).
fn painted_cells(s: &str) -> Vec<(u16, u16, String)> {
    let mut out = Vec::new();
    let (mut x, mut y) = (0u16, 0u16);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            let mut body = String::new();
            if chars.peek() == Some(&'[') {
                chars.next();
            }
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    if c == 'H'
                        && let Some((r, col)) = body.split_once(';')
                        && let (Ok(rn), Ok(cn)) = (r.parse::<u16>(), col.parse::<u16>())
                    {
                        y = rn.saturating_sub(1);
                        x = cn.saturating_sub(1);
                    }
                    break;
                }
                body.push(c);
            }
            continue;
        }
        let w = u16::try_from(crate::render::cell_width(c).unwrap_or(0)).unwrap_or(1);
        if w == 0 {
            if let Some(last) = out.last_mut() {
                let (_, _, sym): &mut (u16, u16, String) = last;
                sym.push(c);
            }
            continue;
        }
        out.push((x, y, c.to_string()));
        x = x.saturating_add(w);
    }
    out
}

/// Every CUP target `(row_1b, col_1b)` in a VT stream.
fn extract_cups(s: &str) -> Vec<(u16, u16)> {
    s.split("\x1b[")
        .skip(1)
        .filter_map(|seq| {
            let end = seq.find(|c: char| c.is_ascii_alphabetic())?;
            if !seq[end..].starts_with('H') {
                return None;
            }
            let (r, c) = seq[..end].split_once(';')?;
            Some((r.parse().ok()?, c.parse().ok()?))
        })
        .collect()
}
