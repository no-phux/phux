//! Reusable themed overlay primitives (phux-ahv.5).
//!
//! [`Modal`] is the centered bordered box every overlay ([`prompt`], the
//! action finder, pickers) paints through, plus the shared geometry and
//! scrollbar helpers. It renders into a ratatui [`Buffer`] and owns (copies)
//! its [`Theme`] so the overlay that holds it stays `'static`.
//!
//! [`prompt`]: super::prompt
//! [`Block`]: ratatui::widgets::Block
//! [`Paragraph`]: ratatui::widgets::Paragraph

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Widget, Wrap};

use crate::render::{ChromeBreakpoints, Theme};

/// Horizontal inner padding of a floating modal, in cells. Text never sits
/// flush against the border.
pub const MODAL_PAD: u16 = 1;

/// Interior text width of a modal: two border columns plus [`MODAL_PAD`]
/// on each side.
#[must_use]
pub const fn modal_inner_width(area_width: u16) -> u16 {
    area_width.saturating_sub(2 + MODAL_PAD * 2)
}

/// A centered, bordered modal box: themed border + left title and a body
/// of pre-built [`Line`]s.
///
/// The caller supplies the body content (already styled) and the
/// [`Modal`] owns the chrome — border color from [`Theme::border`], title
/// from [`Theme::accent`]. Render with
/// [`Modal::render_into`], passing the modal rect (use [`centered`] to
/// compute one).
#[derive(Debug, Clone)]
pub struct Modal<'a> {
    theme: Theme,
    title: String,
    body: Vec<Line<'a>>,
    wrap: bool,
}

impl<'a> Modal<'a> {
    /// A modal titled `title` with `body` lines. Body wrapping
    /// off by default (use [`Self::wrap`] to enable). Title is rendered
    /// left-aligned as ` title ` in the border.
    #[must_use]
    pub fn new(theme: &Theme, title: impl Into<String>, body: Vec<Line<'a>>) -> Self {
        Self {
            theme: *theme,
            title: title.into(),
            body,
            wrap: false,
        }
    }

    /// Enable word wrapping of the body (preserving leading whitespace).
    #[must_use]
    pub const fn wrap(mut self, wrap: bool) -> Self {
        self.wrap = wrap;
        self
    }

    /// Paint the modal into `buf`, filling `area` (the modal rect — the
    /// caller centers it). Border + title chrome come from the theme;
    /// body lines are painted as-is.
    pub fn render_into(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            // Fill the box with the theme surface so the modal reads as a
            // solid panel floating over the live panes rather than as
            // text that appeared in the grid. phux cannot DIM the
            // backdrop the way a single-buffer TUI can — the pane cells
            // belong to libghostty and the chrome never re-emits them
            // (ADR-0020) — so the panel's own contrast plus the drop
            // shadow are what separate it from what is behind it. Set
            // `[theme] surface = "reset"` for a transparent modal.
            .style(Style::default().fg(self.theme.text).bg(self.theme.surface))
            .border_style(Style::default().fg(self.theme.border))
            .title(Span::styled(
                format!(" {} ", self.title),
                Style::default().fg(self.theme.accent),
            ))
            .title_alignment(Alignment::Left)
            .padding(Padding::horizontal(MODAL_PAD));

        let mut para = Paragraph::new(self.body.clone()).block(block);
        if self.wrap {
            para = para.wrap(Wrap { trim: false });
        }
        para.render(area, buf);
    }
}

/// Compute a centered [`Rect`] inside `outer`.
///
/// Sized to `frac_num`/10 of the outer dimensions, clamped to at least
/// `min_w`×`min_h` (themselves clamped to the outer bounds so tiny
/// terminals still show something) and never exceeding `outer`.
///
/// Prefer [`centered_panel`] for anything with content to lay out — it
/// adds the small-viewport behaviour and degrades to this on a roomy one.
#[must_use]
pub fn centered(outer: Rect, frac_num: u16, min_w: u16, min_h: u16) -> Rect {
    let w = outer.width.saturating_mul(frac_num) / 10;
    let h = outer.height.saturating_mul(frac_num) / 10;
    let w = w.clamp(min_w.min(outer.width), outer.width);
    let h = h.clamp(min_h.min(outer.height), outer.height);
    let x = outer.x + (outer.width.saturating_sub(w)) / 2;
    let y = outer.y + (outer.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

/// Whether `outer` is starved on either axis — the one breakpoint the
/// whole chrome shares, so "compact" means the same thing to the status
/// bar, the sidebar, and every overlay.
///
/// `bp` is the per-attach snapshot of `[chrome]` (phux-huhi); pass
/// [`ChromeBreakpoints::default`] where there is no config to consult.
#[must_use]
pub const fn is_compact(outer: Rect, bp: ChromeBreakpoints) -> bool {
    bp.is_col_starved(outer.width) || bp.is_row_starved(outer.height)
}

/// [`centered`], going full-bleed on whichever axis is starved.
///
/// This is the responsive modal geometry. On a roomy viewport it is
/// exactly [`centered`]: a floating box with panes visible around it,
/// which is what makes an overlay feel like it is *over* your work rather
/// than instead of it. On a cramped one those margins are the difference
/// between a readable picker and a two-word column, so the box takes the
/// whole axis and the modal becomes a screen.
///
/// The axes are decided independently on purpose. A short, wide terminal
/// (a bottom-docked split, say) is row-starved but not column-starved: it
/// wants full height and a centered width, not a stretched-out list of
/// two-word rows.
///
/// `bp` carries the thresholds, so a user who moved them in `[chrome]`
/// moves this decision with them.
#[must_use]
pub fn centered_panel(
    outer: Rect,
    frac_num: u16,
    min_w: u16,
    min_h: u16,
    bp: ChromeBreakpoints,
) -> Rect {
    let mut r = centered(outer, frac_num, min_w, min_h);
    if bp.is_col_starved(outer.width) {
        r.x = outer.x;
        r.width = outer.width;
    }
    if bp.is_row_starved(outer.height) {
        r.y = outer.y;
        r.height = outer.height;
    }
    r
}

/// Scroll `offset` by the minimum needed to bring row `cursor` inside a
/// `height`-row window over `total` rows, and clamp it to the content.
///
/// This is the "scroll into view" rule every list widget wants: the window
/// does not move while the cursor stays inside it, so paging down through a
/// long list scrolls one row at a time at the bottom edge and the view holds
/// still in the middle. Returns the new first-visible row. A window that can
/// show everything (`total <= height`) always sits at `0`.
#[must_use]
pub const fn scroll_into_view(offset: usize, cursor: usize, total: usize, height: usize) -> usize {
    if height == 0 || total <= height {
        return 0;
    }
    // Clamp first: a shrinking list (or a narrowing filter) can strand the
    // offset past the end, which would paint a window of blank rows.
    let max_offset = total - height;
    let mut offset = if offset > max_offset {
        max_offset
    } else {
        offset
    };
    if cursor < offset {
        offset = cursor;
    } else if cursor >= offset + height {
        offset = cursor + 1 - height;
    }
    offset
}

/// Paint a vertical scrollbar into `track` — a one-column [`Rect`], meant to
/// be the modal's right *border* column beside the scrolling region.
///
/// The thumb (a block glyph in [`Theme::dim`]) is sized to the visible
/// fraction of `total` and positioned by `offset`, so it reads as both "how
/// much list is there" and "where am I in it". Track cells keep the border
/// glyph in [`Theme::border`], so the bar looks like part of the box rather
/// than a widget bolted onto it. No-op when the content fits (`total <=
/// track.height`) — an unscrollable list shows a plain border.
pub fn paint_scrollbar(buf: &mut Buffer, track: Rect, theme: &Theme, total: usize, offset: usize) {
    let height = track.height as usize;
    if track.width == 0 || height == 0 || total <= height {
        return;
    }
    // Thumb length is the visible fraction of the content, never zero (a
    // 500-row list in a 4-row window still needs something to grab onto).
    let thumb_len = (height * height / total).max(1);
    // The thumb travels `height - thumb_len` rows as the offset travels
    // `total - height` rows, so both ends land exactly flush.
    let travel = height - thumb_len;
    let max_offset = total - height;
    let thumb_top = offset.min(max_offset) * travel / max_offset;

    let thumb = Style::default().fg(theme.dim).bg(theme.surface);
    let rail = Style::default().fg(theme.border).bg(theme.surface);
    for row in 0..track.height {
        let on_thumb = {
            let row = usize::from(row);
            row >= thumb_top && row < thumb_top + thumb_len
        };
        if let Some(cell) = buf.cell_mut((track.x, track.y + row)) {
            cell.set_symbol(if on_thumb { "█" } else { "│" });
            cell.set_style(if on_thumb { thumb } else { rail });
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    /// Flatten a rendered buffer to a `\n`-joined string with trailing
    /// spaces trimmed per row.
    fn buf_to_string(buf: &Buffer) -> String {
        let area = buf.area;
        let mut out = String::new();
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                row.push_str(buf[(area.x + x, area.y + y)].symbol());
            }
            out.push_str(row.trim_end());
            out.push('\n');
        }
        out
    }

    fn render_modal(modal: &Modal<'_>, w: u16, h: u16) -> String {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        modal.render_into(area, &mut buf);
        buf_to_string(&buf)
    }

    #[test]
    fn modal_renders_title_and_body() {
        let theme = Theme::default();
        let modal = Modal::new(
            &theme,
            "demo",
            vec![Line::from("hello"), Line::from("world")],
        );
        let text = render_modal(&modal, 40, 10);
        assert!(text.contains("demo"), "title:\n{text}");
        assert!(text.contains("hello"), "body line 1:\n{text}");
        assert!(text.contains("world"), "body line 2:\n{text}");
    }

    #[test]
    fn modal_byte_output_is_stable() {
        let theme = Theme::default();
        let modal = Modal::new(&theme, "box", vec![Line::from("body")]);
        let area = Rect::new(0, 0, 16, 5);
        let mut buf = Buffer::empty(area);
        modal.render_into(area, &mut buf);
        insta::assert_snapshot!(buf_to_string(&buf));
    }

    // ---------- phux-ep9s: scroll viewport + scrollbar ----------

    #[test]
    fn scroll_into_view_pins_to_zero_when_everything_fits() {
        // No window movement is possible (or wanted) while the content fits,
        // wherever the cursor is — an unscrollable list never scrolls.
        assert_eq!(scroll_into_view(0, 0, 3, 10), 0);
        assert_eq!(scroll_into_view(0, 2, 3, 10), 0);
        // Even a stale non-zero offset (list shrank under it) snaps back.
        assert_eq!(scroll_into_view(7, 2, 3, 10), 0);
    }

    #[test]
    fn scroll_into_view_holds_still_while_the_cursor_is_inside() {
        // Window [5, 10) over 100 rows: a cursor anywhere inside it must not
        // move the view. This is the property that makes the list feel calm.
        for cursor in 5..10 {
            assert_eq!(
                scroll_into_view(5, cursor, 100, 5),
                5,
                "cursor {cursor} inside the window must not scroll it",
            );
        }
    }

    #[test]
    fn scroll_into_view_follows_the_cursor_off_each_edge() {
        // Off the bottom: scroll just enough to put the cursor on the last row.
        assert_eq!(scroll_into_view(5, 10, 100, 5), 6);
        // Off the top: scroll just enough to put it on the first row.
        assert_eq!(scroll_into_view(5, 3, 100, 5), 3);
        // A jump to the end (End key) lands the window flush with the bottom.
        assert_eq!(scroll_into_view(0, 99, 100, 5), 95);
    }

    #[test]
    fn scroll_into_view_clamps_a_stranded_offset() {
        // The filter narrowed 100 rows to 8 while the offset sat at 90: the
        // window must clamp to the content, not paint 5 blank rows.
        assert_eq!(scroll_into_view(90, 0, 8, 5), 0);
        assert_eq!(scroll_into_view(90, 7, 8, 5), 3);
        // A zero-height viewport is degenerate, not a panic.
        assert_eq!(scroll_into_view(4, 9, 100, 0), 0);
    }

    /// Read the scrollbar track column out of a buffer as a string.
    fn track_column(buf: &Buffer, track: Rect) -> String {
        (0..track.height)
            .map(|row| buf[(track.x, track.y + row)].symbol().to_owned())
            .collect()
    }

    #[test]
    fn scrollbar_is_absent_when_the_content_fits() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 8));
        let track = Rect::new(3, 0, 1, 8);
        paint_scrollbar(&mut buf, track, &Theme::default(), 8, 0);
        // Untouched: the cells keep the buffer's default blank symbol, so the
        // modal's plain border shows through.
        assert_eq!(track_column(&buf, track), " ".repeat(8));
    }

    #[test]
    fn scrollbar_thumb_tracks_the_offset() {
        let theme = Theme::default();
        let track = Rect::new(3, 0, 1, 8);
        // 8-row window over 32 rows ⇒ thumb is a quarter of the track (2 rows),
        // travelling 6 rows as the offset travels 24.
        let paint = |offset: usize| {
            let mut buf = Buffer::empty(Rect::new(0, 0, 4, 8));
            paint_scrollbar(&mut buf, track, &theme, 32, offset);
            track_column(&buf, track)
        };
        // At the top the thumb is flush with the first row...
        assert_eq!(paint(0), "██││││││");
        // ...at the bottom, flush with the last (so "am I at the end?" is
        // answerable at a glance)...
        assert_eq!(paint(24), "││││││██");
        // ...and in between it sits proportionally.
        assert_eq!(paint(12), "│││██│││");
    }

    #[test]
    fn scrollbar_thumb_never_vanishes_on_a_long_list() {
        // 4-row window over 500 rows: the proportional thumb rounds to zero
        // rows, but a scrollbar you cannot see is not a scrollbar.
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 4));
        let track = Rect::new(1, 0, 1, 4);
        paint_scrollbar(&mut buf, track, &Theme::default(), 500, 0);
        assert_eq!(
            track_column(&buf, track).matches('█').count(),
            1,
            "the thumb must stay at least one row tall",
        );
    }

    #[test]
    fn scrollbar_ignores_a_degenerate_track() {
        // Zero-width / zero-height tracks are a no-op, not an index panic.
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 4));
        paint_scrollbar(&mut buf, Rect::new(3, 0, 0, 4), &Theme::default(), 99, 0);
        paint_scrollbar(&mut buf, Rect::new(3, 0, 1, 0), &Theme::default(), 99, 0);
    }

    /// Above the breakpoint on both axes, `centered_panel` is exactly
    /// `centered`: the box floats and the panes stay visible around it.
    #[test]
    fn a_roomy_viewport_keeps_the_modal_floating() {
        let bp = ChromeBreakpoints::DEFAULT;
        let outer = Rect::new(0, 0, 120, 40);
        assert!(!is_compact(outer, bp));
        assert_eq!(
            centered_panel(outer, 6, 30, 10, bp),
            centered(outer, 6, 30, 10)
        );
        let r = centered_panel(outer, 6, 30, 10, bp);
        assert!(r.x > outer.x && r.y > outer.y);
        assert!(r.width < outer.width && r.height < outer.height);
    }

    /// A viewport starved on both axes gives the modal the whole screen —
    /// on a 50x14 terminal a 60% box is 30x8, and the six rows of shared
    /// modal chrome leave two rows of actual content.
    #[test]
    fn a_starved_viewport_makes_the_modal_full_bleed() {
        let bp = ChromeBreakpoints::DEFAULT;
        let outer = Rect::new(0, 0, 50, 14);
        assert!(is_compact(outer, bp));
        assert_eq!(centered_panel(outer, 6, 30, 10, bp), outer);
    }

    /// phux-huhi: the breakpoint is the caller's, not a constant. The same
    /// 80x30 viewport floats under the shipped thresholds and goes
    /// full-bleed under a `[chrome]` that raised them — which is the whole
    /// point of the knob for someone who wants full-bleed pickers on a
    /// roomier terminal.
    #[test]
    fn a_raised_breakpoint_moves_where_full_bleed_starts() {
        let outer = Rect::new(0, 0, 80, 30);
        let shipped = ChromeBreakpoints::DEFAULT;
        assert!(!is_compact(outer, shipped));
        assert_ne!(centered_panel(outer, 6, 30, 10, shipped), outer);

        let roomy = ChromeBreakpoints {
            compact_cols: 100,
            compact_rows: 40,
            ..ChromeBreakpoints::DEFAULT
        };
        assert!(is_compact(outer, roomy));
        assert_eq!(centered_panel(outer, 6, 30, 10, roomy), outer);
    }

    /// The axes are decided independently: a short, wide viewport wants
    /// full height and a centered width, not a stretched row of two-word
    /// entries.
    #[test]
    fn each_axis_goes_full_bleed_on_its_own() {
        let bp = ChromeBreakpoints::DEFAULT;
        // Wide but short.
        let short = Rect::new(0, 0, 160, 12);
        let r = centered_panel(short, 6, 30, 10, bp);
        assert_eq!((r.y, r.height), (short.y, short.height), "full height");
        assert!(r.width < short.width, "width still floats: {r:?}");

        // Narrow but tall.
        let narrow = Rect::new(0, 0, 40, 60);
        let r = centered_panel(narrow, 6, 30, 10, bp);
        assert_eq!((r.x, r.width), (narrow.x, narrow.width), "full width");
        assert!(r.height < narrow.height, "height still floats: {r:?}");
    }

    /// `centered_panel` respects an inset outer rect (the pane content
    /// area beside a docked sidebar): full-bleed means "fills what it was
    /// given", never "fills the terminal".
    #[test]
    fn full_bleed_stays_inside_an_inset_outer_rect() {
        // 60-col viewport with a 20-col left sidebar ⇒ content x∈[20, 60).
        let content = Rect::new(20, 0, 40, 14);
        let r = centered_panel(content, 6, 30, 10, ChromeBreakpoints::DEFAULT);
        assert_eq!(r, content);
        assert_eq!(r.x, 20, "must not paint over the sidebar strip");
    }

    #[test]
    fn centered_clamps_to_outer() {
        let outer = Rect::new(0, 0, 20, 8);
        let inner = centered(outer, 7, 40, 10);
        assert!(inner.width <= outer.width);
        assert!(inner.height <= outer.height);
        assert!(inner.x + inner.width <= outer.x + outer.width);
        assert!(inner.y + inner.height <= outer.y + outer.height);
    }

    /// phux-foz.14: when the outer rect is the pane content rect (viewport
    /// inset by a left sidebar strip), the centered modal must stay fully
    /// inside it — its left edge lands right of the sidebar divider, never on
    /// the strip columns. This is the exact geometry the floating-modal path
    /// now feeds `centered`.
    #[test]
    fn centered_against_inset_rect_clears_the_sidebar() {
        // 80-col viewport, a 20-col left sidebar ⇒ content rect x∈[20, 80).
        let sidebar_w = 20;
        let content = Rect::new(sidebar_w, 0, 80 - sidebar_w, 24);
        let modal = centered(content, 6, 30, 10);
        // Fully within the content rect on every edge.
        assert!(
            modal.x >= content.x,
            "modal left edge {} must not enter the sidebar (divider at {})",
            modal.x,
            content.x
        );
        assert!(modal.x + modal.width <= content.x + content.width);
        assert!(modal.y >= content.y);
        assert!(modal.y + modal.height <= content.y + content.height);
        // And horizontally centered *within the content rect*, not the raw
        // viewport: the left and right margins inside the content match.
        let left_margin = modal.x - content.x;
        let right_margin = (content.x + content.width) - (modal.x + modal.width);
        assert!(
            left_margin.abs_diff(right_margin) <= 1,
            "modal must be centered in the content rect: L={left_margin} R={right_margin}"
        );
    }
}
