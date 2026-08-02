//! Compose the client's multi-pane view into dense structured cells
//! (`phux snapshot --rendered`, phux-l5xa).
//!
//! This is the structured-cells counterpart to `paint::paint_full_frame`:
//! where that emits VT to the outer terminal, `compose_full_frame_cells`
//! assembles the *same* frame — pane content (tiled per the layout),
//! dividers, and the status bar — into a [`RenderedFrame`] of dense
//! grapheme + style cells, so an agent, a test, or an assistant debugging a
//! composition bug can ask "what does the assembled screen look like right
//! now" and get cells back with no external emulator in the loop.
//!
//! The composition order mirrors `paint_full_frame` exactly so the two stay
//! in agreement:
//!
//! 1. Reserve the bar row (`pane_viewport`) and tile the panes
//!    (`compute_layout`) into the remaining viewport.
//! 2. Fill every pane's cells via [`super::render::TerminalRenderer::render_at_cells`].
//! 3. Overlay the divider glyphs (read from the same ratatui `Buffer` the VT
//!    path emits).
//! 4. Overlay the status bar onto its reserved row.
//! 5. Adopt the focused pane's cursor as the frame cursor — the focused pane
//!    owns final cursor placement in the VT path, so the rendered frame
//!    reports the same.

use std::collections::HashMap;
use std::time::SystemTime;

use phux_core::screen::{CellColor, CellStyle, RenderedFrame};
use phux_protocol::ids::TerminalId;
use ratatui::buffer::{Buffer, Cell as RatatuiCell, CellDiffOption};
use ratatui::style::{Color, Modifier};

use super::driver::PaneSlot;
use super::paint::{SidebarReservation, bar_inset, content_rect, sidebar_rect};
use crate::layout::LayoutState;
use crate::render::chrome::dividers::compose_buffer as compose_divider_buffer;
use crate::render::chrome::sidebar::SidebarPainter;
use crate::render::chrome::status_bar::{StatusBarPainter, make_context};

/// Compose the assembled multi-pane frame into a dense [`RenderedFrame`].
///
/// `viewport_dims` is the full outer viewport `(cols, rows)`. When a status
/// bar is present its row is reserved (via [`pane_viewport`]) before the
/// panes are tiled, exactly as the live paint does. `now` feeds time-based
/// status widgets so the rendered bar matches a live paint at the same
/// instant.
#[allow(
    clippy::too_many_arguments,
    reason = "phux-4h5a adds the sidebar reservation + painter to the compositor's paint context, mirroring paint_full_frame"
)]
pub(super) fn compose_full_frame_cells(
    layout_state: &LayoutState,
    panes: &mut HashMap<TerminalId, PaneSlot>,
    kernel: &super::driver::AttachKernel,
    focused_pane: Option<&TerminalId>,
    viewport_dims: (u16, u16),
    status_bar: Option<&StatusBarPainter>,
    // phux-4h5a: the window-sidebar reservation. `None` (disabled, the
    // default) makes `content_rect` the full pane viewport, byte-identical to
    // the pre-sidebar tiling. When `Some`, panes inset and `sidebar_painter`
    // overlays the strip into its reserved columns.
    sidebar: Option<SidebarReservation>,
    sidebar_painter: Option<&SidebarPainter>,
    session_name: &str,
    now: SystemTime,
) -> RenderedFrame {
    let (cols, rows) = viewport_dims;
    let bar = status_bar.map(StatusBarPainter::position);
    let content = content_rect(viewport_dims, bar, sidebar);
    let multi = super::multi_pane::compute_layout_in(layout_state, content, viewport_dims);

    let mut frame = RenderedFrame::blank(cols, rows);

    // Fill every visible pane. The focused pane's cursor becomes the frame
    // cursor (it owns final placement in the VT path); other panes' cursors
    // are projected but discarded.
    let mut frame_cursor = None;
    for (id, rect) in &multi.rects {
        let Some(slot) = panes.get_mut(id) else {
            continue;
        };
        let Some(terminal) = super::driver::published_terminal(kernel, id) else {
            continue;
        };
        // A render error on one pane shouldn't sink the whole introspection
        // query; leave that pane's cells blank and move on.
        let Ok(cursor) =
            slot.renderer
                .render_at_cells(terminal, &mut frame, (rect.x, rect.y), (rect.w, rect.h))
        else {
            continue;
        };
        if Some(id) == focused_pane {
            frame_cursor = cursor;
        }
    }

    // Overlay dividers. The divider buffer marks pane interiors `Skip` and
    // carries only the box-drawing glyphs, so overlaying its non-skip,
    // non-blank cells never clobbers pane content.
    let divider_buf = compose_divider_buffer(&multi);
    overlay_buffer(&mut frame, &divider_buf, (0, 0), true);

    // Overlay the sidebar strip into its reserved columns (phux-4h5a). The
    // strip buffer is composed at origin (0,0), so shift it to the reserved
    // rect's x. Its styled blanks/separator are kept (skip_blanks = false) so
    // the strip's full extent shows; it sits in columns `content_rect` carved
    // out, never over pane content.
    if let (Some(res), Some(painter)) = (sidebar, sidebar_painter) {
        let rect = sidebar_rect(viewport_dims, res);
        let strip = painter.compose_buffer(rect);
        overlay_buffer(&mut frame, &strip, (rect.x, rect.y), false);
    }

    // Overlay the status bar onto its reserved row. Styled blanks are kept
    // (an error strip's reverse-video field spans the bar's full span); the bar
    // row sits below the panes, so writing every cell is safe. phux-qtw8: the
    // bar yields the sidebar's columns, so it composes at its own origin —
    // the strip painted above keeps the columns the bar gave up.
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

/// Project a ratatui [`RatatuiCell`]'s style into a plain-data [`CellStyle`].
///
/// The inverse of the chrome layer's `to_ratatui_style`. Color is lossy by
/// nature (a named ANSI color resolves to its palette index 0..=15; ratatui
/// has no overline modifier), which is acceptable for the introspection
/// surface — the structured frame reports *what the chrome painted*, and the
/// chrome's colors are config-sourced names/indices/RGB to begin with.
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

    use crate::attach::driver::PaneSlot;
    use crate::layout::{LayoutState, WindowState, Workspace};
    use crate::render::chrome::status_bar::{Position, StatusBarPainter};
    use phux_protocol::wire::info::{LayoutNode, SplitDir};
    fn published_kernel(entries: &[(&TerminalId, &[u8])]) -> super::super::driver::AttachKernel {
        use phux_client_core::session::{
            EffectBuffer as KernelEffectBuffer, KernelInput, SessionKernel,
        };
        use phux_protocol::{
            BootstrapId, BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, StreamId,
        };

        let terminals = entries
            .iter()
            .map(|(terminal_id, _)| (*terminal_id).clone())
            .collect::<Vec<_>>();
        let mut kernel = SessionKernel::new(
            phux_client_core::engine::ghostty::GhosttyAdapter::new(BootstrapLimits::default()),
            BootstrapProfile::SynthesizedVtRaw,
        );
        let mut effects = KernelEffectBuffer::new();
        kernel
            .update(
                KernelInput::AttachStarted {
                    attach_id: 1,
                    terminals: &terminals,
                },
                &mut effects,
            )
            .expect("attach");
        for (index, (terminal_id, replay)) in entries.iter().enumerate() {
            let stream_id = StreamId::new(1).expect("stream");
            let bootstrap_id = BootstrapId::new(index as u64 + 1).expect("bootstrap");
            kernel
                .update(
                    KernelInput::BootstrapBegin {
                        terminal_id,
                        stream_id,
                        bootstrap_id,
                        profile: BootstrapStreamProfile::SynthesizedVtRaw,
                        geometry: phux_client_core::engine::CanonicalGeometry::new(80, 24)
                            .expect("geometry"),
                        base_seq: 0,
                    },
                    &mut effects,
                )
                .expect("begin");
            kernel
                .update(
                    KernelInput::BootstrapChunk {
                        terminal_id,
                        stream_id,
                        bootstrap_id,
                        chunk_seq: 0,
                        payload: replay,
                    },
                    &mut effects,
                )
                .expect("chunk");
            kernel
                .update(
                    KernelInput::BootstrapReady {
                        terminal_id,
                        stream_id,
                        bootstrap_id,
                        history_cursor: None,
                    },
                    &mut effects,
                )
                .expect("ready");
        }
        kernel
    }

    fn two_pane(left: &TerminalId, right: &TerminalId) -> Workspace {
        Workspace {
            windows: vec![WindowState {
                name: "1".to_owned(),
                state: LayoutState {
                    tree: Some(LayoutNode::Split {
                        dir: SplitDir::Horizontal,
                        ratio: 0.5,
                        left: Box::new(LayoutNode::Leaf(left.clone())),
                        right: Box::new(LayoutNode::Leaf(right.clone())),
                    }),
                    focus: Some(left.clone()),
                },
            }],
            active: 0,
        }
    }

    fn pane_with(bytes: &[u8]) -> PaneSlot {
        let mut slot = PaneSlot::new().expect("pane slot");
        slot.terminal.vt_write(bytes);
        slot
    }

    /// The compositor tiles each pane's content into its rect, draws the
    /// divider between them, and adopts the focused pane's cursor as the
    /// frame cursor — the render-verify harness contract (phux-l5xa).
    #[test]
    fn compose_places_pane_content_divider_and_focused_cursor() {
        let left = TerminalId::local(1);
        let right = TerminalId::local(2);
        let workspace = two_pane(&left, &right);
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        panes.insert(left.clone(), pane_with(b"L"));
        panes.insert(right.clone(), pane_with(b"R"));
        let kernel = published_kernel(&[(&left, b"L"), (&right, b"R")]);

        let frame = compose_full_frame_cells(
            workspace.active_window().expect("active window"),
            &mut panes,
            &kernel,
            Some(&left),
            (80, 24),
            None,
            None,
            None,
            "demo",
            UNIX_EPOCH,
        );

        assert_eq!((frame.cols, frame.rows), (80, 24));
        // No status bar ⇒ panes tile the full viewport; reuse compute_layout
        // to learn each pane's exact origin.
        let multi = super::super::multi_pane::compute_layout(
            workspace.active_window().expect("active window"),
            (80, 24),
        );
        let left_rect = multi.rects.get(&left).copied().expect("left rect");
        let right_rect = multi.rects.get(&right).copied().expect("right rect");
        assert_eq!(
            frame
                .cell(left_rect.y, left_rect.x)
                .expect("left cell")
                .grapheme,
            "L"
        );
        assert_eq!(
            frame
                .cell(right_rect.y, right_rect.x)
                .expect("right cell")
                .grapheme,
            "R"
        );

        // A divider glyph sits in the gap column between the two rects.
        let gap = left_rect.x + left_rect.w;
        let divider = frame.cell(0, gap).expect("gap cell");
        assert!(
            divider.grapheme != " " && !divider.grapheme.is_empty(),
            "expected a divider glyph at the split column {gap}, got {:?}",
            divider.grapheme
        );

        // Focused (left) pane owns the cursor: after "L" at pane col 1.
        let cursor = frame.cursor.expect("frame cursor");
        assert_eq!((cursor.x, cursor.y), (left_rect.x + 1, left_rect.y));
    }

    /// phux-4h5a enabled-path harness: with a Left sidebar reservation the
    /// compositor (a) paints the window strip into its reserved columns
    /// (labels + the `│` separator at the strip's last column), (b) insets the
    /// panes so their content starts at the strip's right edge, and (c) leaves
    /// the disabled (`None`) frame byte-identical to the pre-sidebar tiling.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one test covers the three enabled-path claims (strip / inset / disabled-unchanged) so they share one fixture"
    )]
    fn compose_insets_panes_and_paints_sidebar_strip_when_enabled() {
        use crate::render::Theme;
        use crate::render::chrome::sidebar::SidebarPainter;
        use phux_config::widget::WindowInfo;
        use phux_protocol::ids::TerminalId;

        let left = TerminalId::local(1);
        let right = TerminalId::local(2);
        let workspace = two_pane(&left, &right);
        let ls = workspace.active_window().expect("active window");

        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        panes.insert(left.clone(), pane_with(b"L"));
        panes.insert(right.clone(), pane_with(b"R"));
        let kernel = published_kernel(&[(&left, b"L"), (&right, b"R")]);

        // The same window list `window_infos` would hand the strip painter.
        let mut sidebar_painter = SidebarPainter::new(Theme::default());
        sidebar_painter.set_windows(vec![
            WindowInfo {
                name: "editor".to_owned(),
                active: true,
                zoomed: false,
                attention: false,
                branch: None,
            },
            WindowInfo {
                name: "shell".to_owned(),
                active: false,
                zoomed: false,
                attention: false,
                branch: None,
            },
        ]);

        let res = SidebarReservation {
            edge: super::super::paint::SidebarEdge::Left,
            width: 20,
        };
        let enabled = compose_full_frame_cells(
            ls,
            &mut panes,
            &kernel,
            Some(&left),
            (80, 24),
            None,
            Some(res),
            Some(&sidebar_painter),
            "demo",
            UNIX_EPOCH,
        );

        // (a) The strip occupies columns 0..20. Its separator rule sits in the
        // strip's last column (19); the active window's label glyphs land in
        // the strip's text columns.
        let sep: String = (0..24)
            .filter_map(|r| enabled.cell(r, 19).map(|c| c.grapheme.clone()))
            .collect();
        assert!(
            sep.contains('│'),
            "sidebar separator must sit at the strip's last column (19); got {sep:?}"
        );
        // Row 0 carries the `spaces` section header (phux-foz.9); the
        // first window's name row is row 1.
        let header_text: String = (0..19)
            .filter_map(|c| enabled.cell(0, c).map(|cell| cell.grapheme.clone()))
            .collect();
        assert!(
            header_text.contains("spaces"),
            "the spaces header must top the strip; got {header_text:?}"
        );
        let strip_text: String = (0..19)
            .filter_map(|c| enabled.cell(1, c).map(|cell| cell.grapheme.clone()))
            .collect();
        assert!(
            strip_text.contains("editor"),
            "active window label 'editor' must paint into the strip's text columns; got {strip_text:?}"
        );

        // (b) Panes inset: with a 20-col Left strip, the content rect starts at
        // x = 20, so the focused (left) pane's first cell lands at column 20.
        let content = content_rect((80, 24), None, Some(res));
        let multi = super::super::multi_pane::compute_layout_in(ls, content, (80, 24));
        let left_rect = multi.rects.get(&left).copied().expect("left rect");
        assert_eq!(left_rect.x, 20, "left pane must inset to the strip's edge");
        assert_eq!(
            enabled
                .cell(left_rect.y, left_rect.x)
                .expect("inset left cell")
                .grapheme,
            "L",
            "focused pane content must start at the inset origin (col 20)"
        );
        // Nothing pane-authored bleeds into the strip's columns: col 0 of the
        // pane row is strip text, never the 'L' the pane painted.
        assert_ne!(
            enabled.cell(left_rect.y, 0).expect("col 0").grapheme,
            "L",
            "pane content must not spill into the reserved sidebar columns"
        );

        // (c) The disabled (None) frame is unchanged: panes tile from col 0 and
        // no separator rule is painted in the strip columns.
        let mut panes_off: HashMap<TerminalId, PaneSlot> = HashMap::new();
        panes_off.insert(left.clone(), pane_with(b"L"));
        panes_off.insert(right, pane_with(b"R"));
        let disabled = compose_full_frame_cells(
            ls,
            &mut panes_off,
            &kernel,
            Some(&left),
            (80, 24),
            None,
            None,
            None,
            "demo",
            UNIX_EPOCH,
        );
        let off_multi = super::super::multi_pane::compute_layout(ls, (80, 24));
        let off_left = off_multi.rects.get(&left).copied().expect("off left rect");
        assert_eq!(off_left.x, 0, "disabled path tiles from the origin");
        assert_eq!(
            disabled
                .cell(off_left.y, off_left.x)
                .expect("disabled left cell")
                .grapheme,
            "L"
        );
        // No sidebar separator glyph anywhere on the disabled frame's would-be
        // strip column.
        let off_sep: String = (0..24)
            .filter_map(|r| disabled.cell(r, 19).map(|c| c.grapheme.clone()))
            .collect();
        assert!(
            !off_sep.contains('│'),
            "disabled path must paint no sidebar separator; got {off_sep:?}"
        );
    }

    /// With a status bar the bottom row is reserved and carries the bar
    /// content (here the session name), composited over the panes (phux-l5xa).
    #[test]
    fn compose_overlays_status_bar_on_the_bottom_row() {
        let pane = TerminalId::local(1);
        let workspace = Workspace::single(pane.clone());
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        panes.insert(pane.clone(), pane_with(b"hi"));
        let kernel = published_kernel(&[(&pane, b"hi")]);

        let cfg = phux_config::StatusCfg {
            left: vec![phux_config::Widget::Bare("session-name".to_owned())],
            ..Default::default()
        };
        let reg = phux_config::widget::WidgetRegistry::with_builtins();
        let bar = phux_config::widget::StatusBar::build(&cfg, &reg).expect("bar build");
        let painter = StatusBarPainter::new(bar, Position::Bottom);

        let frame = compose_full_frame_cells(
            workspace.active_window().expect("active window"),
            &mut panes,
            &kernel,
            Some(&pane),
            (80, 24),
            Some(&painter),
            None,
            None,
            "alpha",
            UNIX_EPOCH,
        );

        // The bottom row (23) carries the session name from the bar.
        let bottom: String = (0..frame.cols)
            .filter_map(|c| frame.cell(23, c).map(|cell| cell.grapheme.clone()))
            .collect();
        assert!(
            bottom.contains("alpha"),
            "status-bar row must show the session name, got {bottom:?}"
        );
        // Pane content still lands on the top row (above the reserved bar).
        assert_eq!(frame.cell(0, 0).expect("top-left").grapheme, "h");
    }

    /// phux-qtw8: with the sidebar open the bar row belongs to the STRIP in the
    /// strip's columns — the window tabs start beside it, not underneath. This
    /// is the reported bug, asserted on the assembled frame: before the fix the
    /// tabs composed from column 0 and read as sitting "under" the sidebar.
    #[test]
    fn compose_keeps_the_bar_out_of_the_sidebar_columns() {
        use crate::render::Theme;
        use phux_config::widget::WindowInfo;

        let pane = TerminalId::local(1);
        let workspace = Workspace::single(pane.clone());
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        panes.insert(pane.clone(), pane_with(b"hi"));
        let kernel = published_kernel(&[(&pane, b"hi")]);

        // A bar whose ONLY widget is the window tabs, left-slotted — the strip
        // that was landing under the sidebar.
        let cfg = phux_config::StatusCfg {
            left: vec![phux_config::Widget::Bare("windows".to_owned())],
            ..Default::default()
        };
        let reg = phux_config::widget::WidgetRegistry::with_builtins();
        let bar = phux_config::widget::StatusBar::build(&cfg, &reg).expect("bar build");
        let mut painter = StatusBarPainter::new(bar, Position::Bottom);
        let windows = vec![WindowInfo {
            name: "editor".to_owned(),
            active: true,
            zoomed: false,
            attention: false,
            branch: None,
        }];
        painter.set_windows(windows.clone());

        let mut sidebar_painter = SidebarPainter::new(Theme::default());
        sidebar_painter.set_windows(windows);

        let strip = SidebarReservation {
            edge: super::super::paint::SidebarEdge::Left,
            width: 20,
        };
        let frame = compose_full_frame_cells(
            workspace.active_window().expect("active window"),
            &mut panes,
            &kernel,
            Some(&pane),
            (80, 24),
            Some(&painter),
            Some(strip),
            Some(&sidebar_painter),
            "alpha",
            UNIX_EPOCH,
        );

        // The bar row's first 20 columns are the strip's, so the tab text
        // ("0:editor") must not appear there.
        let under_strip: String = (0..20)
            .filter_map(|c| frame.cell(23, c).map(|cell| cell.grapheme.clone()))
            .collect();
        assert!(
            !under_strip.contains("editor"),
            "window tabs must not paint under the sidebar; got {under_strip:?}"
        );
        // The strip owns the bar row outright: full-height means its
        // bottom-corner collapse chevron now lands ON that row (23, 19) — the
        // cell the bar used to cover.
        assert_eq!(
            frame
                .cell(23, 19)
                .expect("strip bottom corner")
                .grapheme
                .as_str(),
            crate::render::chrome::sidebar::COLLAPSE_GLYPH,
            "the full-height strip's collapse chevron owns the bar row's corner"
        );
        // The tabs themselves live beside the strip, from column 20 on.
        let beside_strip: String = (20..80)
            .filter_map(|c| frame.cell(23, c).map(|cell| cell.grapheme.clone()))
            .collect();
        assert!(
            beside_strip.contains("editor"),
            "window tabs must paint beside the strip; got {beside_strip:?}"
        );
    }
}
