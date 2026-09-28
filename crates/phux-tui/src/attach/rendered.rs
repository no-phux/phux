//! Compose the client's multi-pane view into dense structured cells
//! (`phux snapshot --rendered`).
//!
//! The structured-cells counterpart to `paint::paint_full_frame`, in the same
//! order so the two agree: tile the panes into the content rect and fill
//! their cells, overlay dividers, the sidebar strip, and the status bar, and
//! adopt the focused pane's cursor as the frame cursor.

use std::collections::HashMap;
use std::time::SystemTime;

use phux_core::screen::{CellColor, CellStyle, RenderedFrame};
use phux_protocol::ids::ResourceId;
use ratatui::buffer::{Buffer, Cell as RatatuiCell, CellDiffOption};
use ratatui::style::{Color, Modifier};

#[cfg(test)]
use super::paint::content_rect;
use super::paint::{ContentLayout, SidebarReservation, bar_inset, content_layout, sidebar_rect};
use super::pane_state::PaneSlot;
use crate::layout::LayoutState;
use crate::render::chrome::dividers::compose_buffer as compose_divider_buffer;
use crate::render::chrome::sidebar::SidebarPainter;
use crate::render::chrome::status_bar::{StatusBarPainter, make_context};

/// Compose the assembled multi-pane frame into a dense [`RenderedFrame`] for
/// the outer viewport `viewport_dims`. `now` feeds time-based status widgets.
#[allow(
    clippy::too_many_arguments,
    reason = "the paint context is passed flat, mirroring paint_full_frame"
)]
pub(super) fn compose_full_frame_cells(
    layout_state: &LayoutState,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    kernel: &super::pane_state::AttachKernel,
    focused_resource: Option<&ResourceId>,
    viewport_dims: (u16, u16),
    status_bar: Option<&StatusBarPainter>,
    sidebar: Option<SidebarReservation>,
    sidebar_painter: Option<&SidebarPainter>,
    session_name: &str,
    now: SystemTime,
    theme: &crate::render::theme::Theme,
) -> RenderedFrame {
    let (cols, rows) = viewport_dims;
    let bar = status_bar.map(StatusBarPainter::position);
    let ContentLayout {
        rect: content,
        rail,
    } = content_layout(viewport_dims, bar, sidebar);
    let multi = super::multi_pane::compute_layout_in(layout_state, content, viewport_dims);

    let mut frame = RenderedFrame::blank(cols, rows);

    // The focused pane's cursor becomes the frame cursor.
    let mut frame_cursor = None;
    for (id, rect) in &multi.rects {
        let Some(slot) = panes.get_mut(id) else {
            continue;
        };
        let Some(walk) = super::pane_state::published_replica(kernel, id) else {
            continue;
        };
        // One pane's render error leaves its cells blank.
        let Ok(cursor) =
            slot.renderer
                .render_at_cells(walk, &mut frame, (rect.x, rect.y), (rect.w, rect.h))
        else {
            continue;
        };
        if Some(id) == focused_resource {
            frame_cursor = cursor;
        }
    }

    // Divider interiors are `Skip`, so overlaying never clobbers panes.
    let divider_buf = {
        let panes_ref = &*panes;
        compose_divider_buffer(&multi, content, rail, focused_resource, theme, |id| {
            super::pane_state::pane_label(panes_ref, id)
        })
    };
    overlay_buffer(&mut frame, &divider_buf, (0, 0), true);

    // The strip, shifted to its reserved columns; styled blanks are kept.
    if let (Some(res), Some(painter)) = (sidebar, sidebar_painter) {
        let rect = sidebar_rect(viewport_dims, res);
        let strip = painter.compose_buffer(rect, super::paint::sidebar_rule(res.edge), rail);
        overlay_buffer(&mut frame, &strip, (rect.x, rect.y), false);
    }

    // The status bar at its own origin (it yields the strip's columns);
    // styled blanks kept so an error strip spans the bar.
    if let Some(painter) = status_bar {
        let ctx = make_context(session_name, now);
        if let Some((bar_buf, x, row_index)) =
            painter.compose_buffer(bar_inset(viewport_dims, sidebar), cols, rows, &ctx)
        {
            overlay_buffer(&mut frame, &bar_buf, (x, row_index), false);
        }
    }

    frame.cursor = frame_cursor;
    frame
}

/// Overlay a ratatui [`Buffer`] onto `frame`, shifting the buffer's rows by
/// `row_offset`. `Skip` cells are never written (the libghostty pane owns
/// them). When `skip_blanks` is set, empty/space cells are also skipped so a
/// divider buffer's gap cells don't paint over pane content; the status bar
/// passes `false` so its styled background spaces survive.
fn overlay_buffer(frame: &mut RenderedFrame, buf: &Buffer, origin: (u16, u16), skip_blanks: bool) {
    let (col_offset, row_offset) = origin;
    let area = buf.area;
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            let Some(cell) = buf.cell((x, y)) else {
                continue;
            };
            if cell.diff_option == CellDiffOption::Skip {
                continue;
            }
            let sym = cell.symbol();
            if skip_blanks && (sym.is_empty() || sym == " ") {
                continue;
            }
            if sym.is_empty() {
                continue;
            }
            if let Some(dst) =
                frame.cell_mut(y.saturating_add(row_offset), x.saturating_add(col_offset))
            {
                sym.clone_into(&mut dst.grapheme);
                dst.style = ratatui_cell_to_style(cell);
            }
        }
    }
}

/// Project a ratatui cell's style into a [`CellStyle`] (lossy: named colours
/// become palette indices, and there is no overline).
const fn ratatui_cell_to_style(cell: &RatatuiCell) -> CellStyle {
    let m = cell.modifier;
    CellStyle {
        bold: m.contains(Modifier::BOLD),
        faint: m.contains(Modifier::DIM),
        italic: m.contains(Modifier::ITALIC),
        underline: m.contains(Modifier::UNDERLINED),
        blink: m.contains(Modifier::SLOW_BLINK) || m.contains(Modifier::RAPID_BLINK),
        inverse: m.contains(Modifier::REVERSED),
        invisible: m.contains(Modifier::HIDDEN),
        strikethrough: m.contains(Modifier::CROSSED_OUT),
        // ratatui carries no overline modifier; chrome never sets it.
        overline: false,
        fg: ratatui_color_to_cell(cell.fg),
        bg: ratatui_color_to_cell(cell.bg),
    }
}

/// Project a ratatui [`Color`] into a [`CellColor`]. Named ANSI colors map
/// to their palette index (`0..=15`); `Indexed` keeps its slot; `Rgb` is
/// preserved; `Reset` is the terminal default.
const fn ratatui_color_to_cell(color: Color) -> CellColor {
    match color {
        Color::Reset => CellColor::Default,
        Color::Rgb(r, g, b) => CellColor::Rgb { r, g, b },
        Color::Indexed(index) => CellColor::Palette { index },
        Color::Black => CellColor::Palette { index: 0 },
        Color::Red => CellColor::Palette { index: 1 },
        Color::Green => CellColor::Palette { index: 2 },
        Color::Yellow => CellColor::Palette { index: 3 },
        Color::Blue => CellColor::Palette { index: 4 },
        Color::Magenta => CellColor::Palette { index: 5 },
        Color::Cyan => CellColor::Palette { index: 6 },
        Color::Gray => CellColor::Palette { index: 7 },
        Color::DarkGray => CellColor::Palette { index: 8 },
        Color::LightRed => CellColor::Palette { index: 9 },
        Color::LightGreen => CellColor::Palette { index: 10 },
        Color::LightYellow => CellColor::Palette { index: 11 },
        Color::LightBlue => CellColor::Palette { index: 12 },
        Color::LightMagenta => CellColor::Palette { index: 13 },
        Color::LightCyan => CellColor::Palette { index: 14 },
        Color::White => CellColor::Palette { index: 15 },
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    use crate::attach::paint::SidebarEdge;
    use crate::attach::pane_state::published_test_state;
    use crate::render::Theme;
    use crate::render::chrome::status_bar::Position;
    use phux_config::widget::WindowInfo;
    use phux_protocol::wire::info::{LayoutNode, SplitDir};

    const STRIP: SidebarReservation = SidebarReservation {
        edge: SidebarEdge::Left,
        width: 20,
    };

    fn split(left: &ResourceId, right: &ResourceId) -> LayoutState {
        LayoutState {
            tree: Some(LayoutNode::Split {
                dir: SplitDir::Horizontal,
                ratio: 0.5,
                left: Box::new(LayoutNode::Leaf(left.clone())),
                right: Box::new(LayoutNode::Leaf(right.clone())),
            }),
            focus: Some(left.clone()),
        }
    }

    fn window(name: &str) -> WindowInfo {
        WindowInfo {
            name: name.to_owned(),
            active: name == "editor",
            zoomed: false,
            attention: false,
            branch: None,
            exited: None,
            badge: None,
        }
    }

    fn bar(widget: &str) -> StatusBarPainter {
        let cfg = phux_config::StatusCfg {
            left: vec![phux_config::Widget::Bare(widget.to_owned())],
            ..Default::default()
        };
        let reg = phux_config::widget::WidgetRegistry::with_builtins();
        StatusBarPainter::new(
            phux_config::widget::StatusBar::build(&cfg, &reg).expect("bar build"),
            Position::Bottom,
        )
    }

    fn compose(
        layout: &LayoutState,
        entries: &[(&ResourceId, u16, u16, &[u8])],
        status_bar: Option<&StatusBarPainter>,
        sidebar: Option<(SidebarReservation, &SidebarPainter)>,
    ) -> RenderedFrame {
        let (kernel, _, mut panes) = published_test_state(entries);
        compose_full_frame_cells(
            layout,
            &mut panes,
            &kernel,
            layout.focus.as_ref(),
            (80, 24),
            status_bar,
            sidebar.map(|(res, _)| res),
            sidebar.map(|(_, painter)| painter),
            "alpha",
            UNIX_EPOCH,
            &Theme::default(),
        )
    }

    fn row(frame: &RenderedFrame, r: u16, cols: std::ops::Range<u16>) -> String {
        cols.filter_map(|c| frame.cell(r, c).map(|cell| cell.grapheme.clone()))
            .collect()
    }

    /// Panes tile into their rects with a divider between them, and the
    /// focused pane's cursor is the frame cursor; with a sidebar the strip
    /// paints its columns (separator at col 19) and the panes inset past it,
    /// while without one nothing is reserved.
    #[test]
    fn compose_tiles_panes_divider_cursor_and_sidebar() {
        let (left, right) = (ResourceId::local(1), ResourceId::local(2));
        let layout = split(&left, &right);
        let entries: [(&ResourceId, u16, u16, &[u8]); 2] =
            [(&left, 80, 24, b"L"), (&right, 80, 24, b"R")];
        let mut strip = SidebarPainter::new(Theme::default());
        strip.set_roster(vec![crate::render::chrome::sidebar::SessionRosterEntry {
            name: "test".to_owned(),
            host: "test-host".to_owned(),
            active: true,
            selectable: true,
            ..Default::default()
        }]);
        strip.set_windows(vec![window("editor"), window("shell")]);

        for sidebar in [None, Some(STRIP)] {
            let frame = compose(&layout, &entries, None, sidebar.map(|res| (res, &strip)));
            let multi = crate::attach::multi_pane::compute_layout_in(
                &layout,
                content_rect((80, 24), None, sidebar),
                (80, 24),
            );
            let (l, r) = (multi.rects[&left], multi.rects[&right]);
            assert_eq!(l.x, if sidebar.is_some() { 20 } else { 0 });
            assert_eq!(frame.cell(l.y, l.x).unwrap().grapheme, "L");
            assert_eq!(frame.cell(r.y, r.x).unwrap().grapheme, "R");
            let divider = &frame.cell(l.y, l.x + l.w).unwrap().grapheme;
            assert!(divider != " " && !divider.is_empty(), "{divider:?}");
            let cursor = frame.cursor.clone().expect("frame cursor");
            assert_eq!((cursor.x, cursor.y), (l.x + 1, l.y));

            let separator: String = (0..24).map(|r| row(&frame, r, 19..20)).collect();
            assert_eq!(separator.contains('│'), sidebar.is_some(), "{separator:?}");
            if sidebar.is_some() {
                let strip_rows: Vec<String> = (0..24).map(|r| row(&frame, r, 0..19)).collect();
                assert!(
                    strip_rows.iter().any(|r| r.contains("Sessions")),
                    "{strip_rows:?}"
                );
                assert!(
                    strip_rows.iter().any(|r| r.contains("editor")),
                    "{strip_rows:?}"
                );
                assert_ne!(frame.cell(l.y, 0).unwrap().grapheme, "L");
            }
        }
    }

    /// The status bar composes onto the reserved bottom row, beside (never
    /// under) a sidebar whose full-height strip owns the corner cell.
    #[test]
    fn compose_places_the_bar_beside_the_sidebar() {
        let pane = ResourceId::local(1);
        let layout = LayoutState {
            tree: Some(LayoutNode::Leaf(pane.clone())),
            focus: Some(pane.clone()),
        };
        let entries: [(&ResourceId, u16, u16, &[u8]); 1] = [(&pane, 80, 24, b"hi")];
        let frame = compose(&layout, &entries, Some(&bar("session-name")), None);
        assert!(row(&frame, 23, 0..80).contains("alpha"));
        assert_eq!(frame.cell(1, 0).unwrap().grapheme, "h", "row 0 is the rail");

        let mut tabs = bar("windows");
        tabs.set_windows(vec![window("editor")]);
        let mut strip = SidebarPainter::new(Theme::default());
        strip.set_windows(vec![window("editor")]);
        let frame = compose(&layout, &entries, Some(&tabs), Some((STRIP, &strip)));
        assert!(
            !row(&frame, 23, 0..20).contains("editor"),
            "tabs must not paint under the strip"
        );
        assert_eq!(
            frame.cell(23, 19).unwrap().grapheme.as_str(),
            crate::render::chrome::sidebar::COLLAPSE_GLYPH
        );
        assert!(row(&frame, 23, 20..80).contains("editor"));
    }
}
