//! Reusable themed overlay primitives.
//!
//! [`Modal`] is the centered bordered box every overlay paints through (owning a copy of its [`Theme`] so the
//! overlay stays `'static`), plus shared geometry and scrollbar helpers.

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

/// A centered, bordered modal box: themed border and left title around
/// pre-styled body [`Line`]s. Render with [`Modal::render_into`] into a rect
/// from [`centered`] or [`centered_panel`].
#[derive(Debug, Clone)]
pub struct Modal<'a> {
    theme: Theme,
    title: String,
    body: Vec<Line<'a>>,
    wrap: bool,
}

impl<'a> Modal<'a> {
    /// A modal titled `title` over `body`, wrapping off (see [`Self::wrap`]).
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

    /// Paint the modal into `area` (the already-centered modal rect).
    pub fn render_into(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            // Fill with the theme surface: the pane cells behind cannot be
            // dimmed (ADR-0020), so the panel's contrast and the drop shadow
            // separate it. `[theme] surface = "reset"` makes it transparent.
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

/// A centered [`Rect`] at `frac_num`/10 of `outer`, at least `min_w`x`min_h`
/// (clamped to `outer`). Prefer [`centered_panel`] for content.
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

/// Whether `outer` is starved on either axis under `bp` (the one breakpoint
/// the whole chrome shares).
#[must_use]
pub const fn is_compact(outer: Rect, bp: ChromeBreakpoints) -> bool {
    bp.is_col_starved(outer.width) || bp.is_row_starved(outer.height)
}

/// [`centered`], going full-bleed on each starved axis independently: a
/// floating box on a roomy viewport, a screen on a cramped one (a short, wide
/// viewport gets full height with a centered width).
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

/// Scroll `offset` minimally so row `cursor` is inside a `height`-row window
/// over `total` rows, clamped to the content: the view holds still while the
/// cursor stays inside. `0` when everything fits.
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

/// Paint a scrollbar into `track` (the modal's right border column): a
/// [`Theme::dim`] thumb sized and placed by `total`/`offset` over border-glyph
/// track cells. No-op when the content fits.
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

    #[test]
    fn modal_renders_title_and_body() {
        let theme = Theme::default();
        let modal = Modal::new(
            &theme,
            "demo",
            vec![Line::from("hello"), Line::from("world")],
        );
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        modal.render_into(area, &mut buf);
        let text: String = (0..10)
            .flat_map(|y| (0..40).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].symbol().to_owned())
            .collect();
        assert!(
            ["demo", "hello", "world"].iter().all(|w| text.contains(w)),
            "{text}"
        );
    }

    /// The window holds still while the cursor is inside it, moves minimally
    /// off either edge, and clamps a stranded offset.
    #[test]
    fn scroll_into_view_moves_minimally_and_clamps() {
        for (offset, cursor, total, height, expected) in [
            (0, 2, 3, 10, 0),
            (7, 2, 3, 10, 0),
            (5, 5, 100, 5, 5),
            (5, 9, 100, 5, 5),
            (5, 10, 100, 5, 6),
            (5, 3, 100, 5, 3),
            (0, 99, 100, 5, 95),
            (90, 0, 8, 5, 0),
            (90, 7, 8, 5, 3),
            (4, 9, 100, 0, 0),
        ] {
            assert_eq!(
                scroll_into_view(offset, cursor, total, height),
                expected,
                "offset {offset} cursor {cursor} total {total} height {height}"
            );
        }
    }

    /// The thumb is proportional, flush at both ends, never zero rows, and
    /// absent when the content fits; degenerate tracks are a no-op.
    #[test]
    fn scrollbar_thumb_tracks_the_offset() {
        let theme = Theme::default();
        let track = Rect::new(3, 0, 1, 8);
        let paint = |total: usize, offset: usize| {
            let mut buf = Buffer::empty(Rect::new(0, 0, 4, 8));
            paint_scrollbar(&mut buf, track, &theme, total, offset);
            (0..8)
                .map(|row| buf[(3, row)].symbol().to_owned())
                .collect::<String>()
        };
        assert_eq!(paint(8, 0), " ".repeat(8));
        assert_eq!(paint(32, 0), "██││││││");
        assert_eq!(paint(32, 24), "││││││██");
        assert_eq!(paint(32, 12), "│││██│││");
        assert_eq!(paint(500, 0).matches('█').count(), 1);

        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 4));
        paint_scrollbar(&mut buf, Rect::new(3, 0, 0, 4), &theme, 99, 0);
        paint_scrollbar(&mut buf, Rect::new(3, 0, 1, 0), &theme, 99, 0);
    }

    /// `centered_panel` floats on a roomy viewport and goes full-bleed per
    /// starved axis, under whatever thresholds the caller passes.
    #[test]
    fn centered_panel_floats_or_goes_full_bleed_per_axis() {
        let bp = ChromeBreakpoints::DEFAULT;
        let roomy = Rect::new(0, 0, 120, 40);
        assert!(!is_compact(roomy, bp));
        let r = centered_panel(roomy, 6, 30, 10, bp);
        assert_eq!(r, centered(roomy, 6, 30, 10));
        assert!(r.x > 0 && r.y > 0 && r.width < 120 && r.height < 40);

        let starved = Rect::new(0, 0, 50, 14);
        assert_eq!(centered_panel(starved, 6, 30, 10, bp), starved);
        let tight = ChromeBreakpoints {
            compact_cols: 30,
            compact_rows: 8,
            ..bp
        };
        assert_eq!(
            centered_panel(starved, 6, 30, 10, tight),
            centered(starved, 6, 30, 10)
        );
        let raised = ChromeBreakpoints {
            compact_cols: 100,
            compact_rows: 40,
            ..bp
        };
        let mid = Rect::new(0, 0, 80, 30);
        assert_ne!(centered_panel(mid, 6, 30, 10, bp), mid);
        assert_eq!(centered_panel(mid, 6, 30, 10, raised), mid);

        let short = Rect::new(0, 0, 160, 12);
        let r = centered_panel(short, 6, 30, 10, bp);
        assert!(r.height == 12 && r.width < 160, "{r:?}");
        let narrow = Rect::new(0, 0, 40, 60);
        let r = centered_panel(narrow, 6, 30, 10, bp);
        assert!(r.width == 40 && r.height < 60, "{r:?}");

        // Full-bleed fills the inset rect it was given, never the sidebar.
        let content = Rect::new(20, 0, 40, 14);
        assert_eq!(centered_panel(content, 6, 30, 10, bp), content);
    }

    /// `centered` clamps to its outer rect and centers inside an inset one,
    /// so a modal never lands on the sidebar columns.
    #[test]
    fn centered_clamps_and_centers_in_the_content_rect() {
        let outer = Rect::new(0, 0, 20, 8);
        let inner = centered(outer, 7, 40, 10);
        assert!(inner.right() <= outer.right() && inner.bottom() <= outer.bottom());

        let content = Rect::new(20, 0, 60, 24);
        let modal = centered(content, 6, 30, 10);
        assert!(modal.x >= content.x && modal.right() <= content.right());
        assert!(modal.y >= content.y && modal.bottom() <= content.bottom());
        let (left, right) = (modal.x - content.x, content.right() - modal.right());
        assert!(left.abs_diff(right) <= 1, "L={left} R={right}");
    }
}
