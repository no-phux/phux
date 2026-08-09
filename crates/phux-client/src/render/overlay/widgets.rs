//! Reusable themed overlay primitives (phux-ahv.5).
//!
//! Overlays ([`help`], [`prompt`], and future ones like a command palette)
//! share two visual building blocks:
//!
//! - [`Modal`] — a centered bordered box with a title, body, and optional
//!   footer, styled through [`Theme`] slots (`border`, `accent`, `dim`).
//!   Built on ratatui [`Block`] + [`Paragraph`].
//! - [`KeyChordTable`] — the chord/description columns the help overlay
//!   shows, grouped into titled sections and column-aligned across
//!   section boundaries. Styled through the `chord`, `action`, and
//!   `section_header` slots.
//!
//! Both render into a ratatui [`Buffer`] so they compose with the overlay
//! paint path. They own (copy) their [`Theme`] so the overlay that holds
//! them stays `'static`.
//!
//! [`help`]: super::help
//! [`prompt`]: super::prompt
//! [`Block`]: ratatui::widgets::Block
//! [`Paragraph`]: ratatui::widgets::Paragraph

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

use crate::render::Theme;

/// A centered, bordered modal box: themed border + centered title, a body
/// of pre-built [`Line`]s, and an optional dimmed footer line.
///
/// The caller supplies the body content (already styled) and the
/// [`Modal`] owns the chrome — border color from [`Theme::border`], title
/// from [`Theme::accent`], footer from [`Theme::dim`]. Render with
/// [`Modal::render_into`], passing the modal rect (use [`centered`] to
/// compute one).
#[derive(Debug, Clone)]
pub struct Modal<'a> {
    theme: Theme,
    title: String,
    body: Vec<Line<'a>>,
    footer: Vec<String>,
    wrap: bool,
    scroll: u16,
}

/// Separator between two footer hints. Wide on purpose: the footer is a
/// row of unrelated affordances, and the extra air is what stops them
/// reading as one sentence.
const FOOTER_SEP: &str = "  ·  ";

impl<'a> Modal<'a> {
    /// A modal titled `title` with `body` lines. No footer; body wrapping
    /// off by default (use [`Self::wrap`] to enable). Title is rendered
    /// centered as ` title ` in the border.
    #[must_use]
    pub fn new(theme: &Theme, title: impl Into<String>, body: Vec<Line<'a>>) -> Self {
        Self {
            theme: *theme,
            title: title.into(),
            body,
            footer: Vec::new(),
            wrap: false,
            scroll: 0,
        }
    }

    /// Attach a dimmed, italic footer line painted as the last body row.
    ///
    /// One hint, taken whole. Prefer [`Self::footer_hints`] whenever the
    /// footer is a *list* of affordances — a single pre-joined string
    /// cannot be shortened without cutting a word in half.
    #[must_use]
    pub fn footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = vec![footer.into()];
        self
    }

    /// Attach a footer built from independent hints, most important
    /// first.
    ///
    /// The hints are joined for display, but they are *kept* separate so
    /// a narrow modal can drop whole ones. A footer that reads
    /// `Enter select  ·  Esc cancel  ·  type to fi` has stopped being a
    /// list of affordances and become a rendering artifact; dropping the
    /// last hint costs strictly less.
    #[must_use]
    pub fn footer_hints<S: Into<String>, I: IntoIterator<Item = S>>(mut self, hints: I) -> Self {
        self.footer = hints.into_iter().map(Into::into).collect();
        self
    }

    /// The footer text that fits in `width` interior columns, or `None`
    /// when not even the first hint does.
    fn footer_line(&self, width: usize) -> Option<String> {
        for take in (1..=self.footer.len()).rev() {
            let line = self.footer[..take].join(FOOTER_SEP);
            if line.chars().count() <= width {
                return Some(line);
            }
        }
        None
    }

    /// Enable word wrapping of the body (preserving leading whitespace).
    #[must_use]
    pub const fn wrap(mut self, wrap: bool) -> Self {
        self.wrap = wrap;
        self
    }

    /// Scroll the body down by `rows` display rows.
    ///
    /// With wrapping on ([`Self::wrap`]) the unit is *wrapped* rows —
    /// ratatui's `Paragraph` composes the wrapped lines and skips the
    /// first `rows` of them — so it stays in step with
    /// [`Self::wrapped_row_count`]. The border, title, and footer chrome
    /// scroll with the body (the footer is a body row); the box itself
    /// stays put.
    #[must_use]
    pub const fn scroll(mut self, rows: u16) -> Self {
        self.scroll = rows;
        self
    }

    /// The full painted line set: body plus the footer spacer + footer
    /// row when a footer is set. One source of truth for
    /// [`Self::render_into`] and [`Self::wrapped_row_count`], so the
    /// scroll math counts exactly what the paint path draws.
    /// `width` is the *interior* width (the modal rect less its two
    /// border columns) — the footer needs it to decide how many hints it
    /// can afford.
    fn lines(&self, width: u16) -> Vec<Line<'a>> {
        let mut lines = self.body.clone();
        if let Some(footer) = self.footer_line(width as usize) {
            // Blank spacer + dimmed italic footer, matching the help
            // overlay's prior layout.
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                footer,
                Style::default()
                    .fg(self.theme.dim)
                    .add_modifier(Modifier::ITALIC),
            )));
        }
        lines
    }

    /// Rows the body (footer included) occupies at `width` — counted in
    /// *wrapped* display rows when wrapping is on, logical lines otherwise.
    ///
    /// This is the denominator scrolling needs: a long chord row that
    /// folds onto a second display row consumes two rows of the window,
    /// so counting logical lines would undercount the extent (phux-9adu).
    /// Both this count and [`Self::scroll`] ride ratatui's own word
    /// wrapper (`Paragraph::line_count`), so they can never disagree
    /// with what [`Self::render_into`] paints. `width` is the *interior*
    /// width (the modal rect minus the two border columns).
    #[must_use]
    pub fn wrapped_row_count(&self, width: u16) -> usize {
        let mut para = Paragraph::new(self.lines(width));
        if self.wrap {
            para = para.wrap(Wrap { trim: false });
        }
        // No block attached: `line_count` would add a block's vertical
        // space, but the caller already subtracted the borders from
        // `width`/height, so we count bare text rows.
        para.line_count(width)
    }

    /// Paint the modal into `buf`, filling `area` (the modal rect — the
    /// caller centers it). Border + title chrome come from the theme;
    /// body lines are painted as-is.
    pub fn render_into(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            // Fill the box with the theme surface so the modal reads as a
            // solid panel floating over the live panes. Default `Reset`
            // inherits the terminal background (no visible change).
            .style(Style::default().bg(self.theme.surface))
            .border_style(Style::default().fg(self.theme.border))
            .title(Span::styled(
                format!(" {} ", self.title),
                Style::default()
                    .fg(self.theme.accent)
                    .add_modifier(Modifier::BOLD),
            ))
            .title_alignment(Alignment::Center);

        let mut para = Paragraph::new(self.lines(area.width.saturating_sub(2))).block(block);
        if self.wrap {
            para = para.wrap(Wrap { trim: false });
        }
        if self.scroll > 0 {
            para = para.scroll((self.scroll, 0));
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

/// Viewport width at or below which the chrome is *column-starved*: a
/// floating box's margins stop reading as composition and start reading
/// as columns you cannot use.
///
/// 64 is chosen from the content, not from a round number: a 60% box in
/// 64 columns is 38 wide, and once its two border columns and the two-
/// column indent of a nested picker row are taken out, 34 remain — under
/// the width at which a `session/window` pair plus its branch stays
/// legible. Wider than this and the margins are affordable.
pub const COMPACT_COLS: u16 = 64;

/// Viewport height at or below which the chrome is *row-starved*.
///
/// A 60% box in 18 rows is 10 tall; the shared modal chrome (border,
/// query line and its blank, footer and its blank) spends 6 of them, so
/// four rows of an actual list survive. Below that a picker shows less
/// than a page and scrolling replaces reading.
pub const COMPACT_ROWS: u16 = 18;

/// Whether `outer` is starved on either axis — the one breakpoint the
/// whole chrome shares, so "compact" means the same thing to the status
/// bar, the sidebar, and every overlay.
#[must_use]
pub const fn is_compact(outer: Rect) -> bool {
    outer.width <= COMPACT_COLS || outer.height <= COMPACT_ROWS
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
#[must_use]
pub fn centered_panel(outer: Rect, frac_num: u16, min_w: u16, min_h: u16) -> Rect {
    let mut r = centered(outer, frac_num, min_w, min_h);
    if outer.width <= COMPACT_COLS {
        r.x = outer.x;
        r.width = outer.width;
    }
    if outer.height <= COMPACT_ROWS {
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
/// The thumb (a block glyph in [`Theme::accent`]) is sized to the visible
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

    let thumb = Style::default().fg(theme.accent).bg(theme.surface);
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

/// One row in a [`KeyChordTable`] section: a chord (left column) and its
/// description (right column).
#[derive(Debug, Clone)]
pub struct ChordRow {
    /// The chord as the user types it, e.g. `"C-a v"`.
    pub chord: String,
    /// What it does, e.g. `"split-pane(direction=vertical)"`.
    pub description: String,
}

impl ChordRow {
    /// A row pairing `chord` with `description`.
    #[must_use]
    pub fn new(chord: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            chord: chord.into(),
            description: description.into(),
        }
    }
}

/// A titled group of [`ChordRow`]s.
#[derive(Debug, Clone)]
pub struct ChordSection {
    /// Section heading, e.g. `"Global bindings"`.
    pub title: String,
    /// Rows under this heading.
    pub rows: Vec<ChordRow>,
}

impl ChordSection {
    /// A section titled `title` with `rows`.
    #[must_use]
    pub fn new(title: impl Into<String>, rows: Vec<ChordRow>) -> Self {
        Self {
            title: title.into(),
            rows,
        }
    }
}

/// The chord/description table the help overlay shows.
///
/// Renders sections top-to-bottom (blank spacer between them), each with a
/// bold [`Theme::section_header`] heading, then its rows with the chord
/// column ([`Theme::chord`], bold) padded to align with every other
/// section's chords and the description column ([`Theme::action`]).
///
/// Build with [`KeyChordTable::new`] then turn into renderable body lines
/// with [`KeyChordTable::body_lines`] (so a caller can fold them into a
/// [`Modal`]'s body).
#[derive(Debug, Clone)]
pub struct KeyChordTable {
    theme: Theme,
    sections: Vec<ChordSection>,
    /// Shown when every section is empty (e.g. "No keybindings
    /// configured."), dimmed.
    empty_notice: Option<String>,
}

impl KeyChordTable {
    /// A table over `sections`, styled with `theme`.
    #[must_use]
    pub const fn new(theme: &Theme, sections: Vec<ChordSection>) -> Self {
        Self {
            theme: *theme,
            sections,
            empty_notice: None,
        }
    }

    /// Set the dimmed line shown when no section has any rows.
    #[must_use]
    pub fn empty_notice(mut self, notice: impl Into<String>) -> Self {
        self.empty_notice = Some(notice.into());
        self
    }

    /// Build the body lines: each non-empty section's bold header
    /// followed by its aligned rows, blank-separated. Returns the empty
    /// notice (if set) when no section has rows.
    #[must_use]
    pub fn body_lines(&self) -> Vec<Line<'static>> {
        // Chord column width = longest chord across ALL sections, so the
        // description column lines up through section boundaries. Default
        // of 8 matches the prior help-overlay behavior for the
        // no-bindings case.
        let chord_width = self
            .sections
            .iter()
            .flat_map(|s| s.rows.iter())
            .map(|r| r.chord.len())
            .max()
            .unwrap_or(8);

        let mut lines: Vec<Line<'static>> = Vec::new();
        for section in &self.sections {
            if section.rows.is_empty() {
                continue;
            }
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
            lines.push(Line::from(Span::styled(
                section.title.clone(),
                Style::default()
                    .fg(self.theme.section_header)
                    .add_modifier(Modifier::BOLD),
            )));
            for row in &section.rows {
                lines.push(self.row_line(row, chord_width));
            }
        }

        if lines.is_empty()
            && let Some(notice) = &self.empty_notice
        {
            lines.push(Line::from(Span::styled(
                notice.clone(),
                Style::default().fg(self.theme.dim),
            )));
        }
        lines
    }

    /// One table row: bold chord padded to `width`, two-space gutter, then
    /// the description.
    fn row_line(&self, row: &ChordRow, width: usize) -> Line<'static> {
        let pad = width.saturating_sub(row.chord.len());
        let padding = " ".repeat(pad);
        Line::from(vec![
            Span::styled(
                row.chord.clone(),
                Style::default()
                    .fg(self.theme.chord)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(padding),
            Span::raw("  "),
            Span::styled(
                row.description.clone(),
                Style::default().fg(self.theme.action),
            ),
        ])
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
    fn modal_renders_title_body_and_footer() {
        let theme = Theme::default();
        let modal = Modal::new(
            &theme,
            "demo",
            vec![Line::from("hello"), Line::from("world")],
        )
        .footer("Press Esc to close");
        let text = render_modal(&modal, 40, 10);
        assert!(text.contains("demo"), "title:\n{text}");
        assert!(text.contains("hello"), "body line 1:\n{text}");
        assert!(text.contains("world"), "body line 2:\n{text}");
        assert!(text.contains("Press Esc to close"), "footer:\n{text}");
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

    // ---------- phux-9adu: wrapped-row counting + body scroll ----------

    #[test]
    fn wrapped_row_count_counts_display_rows_not_logical_lines() {
        let theme = Theme::default();
        // One logical line, long enough to fold at a narrow width.
        let body = vec![Line::from("alpha bravo charlie delta")];
        let wrapping = Modal::new(&theme, "t", body.clone()).wrap(true);
        // Wide enough: one display row, same as the logical count.
        assert_eq!(wrapping.wrapped_row_count(40), 1);
        // Narrow: the single logical line folds into several display
        // rows, each of which consumes a row of the scroll window.
        assert!(
            wrapping.wrapped_row_count(8) >= 3,
            "a 25-char line at width 8 must wrap to multiple rows, got {}",
            wrapping.wrapped_row_count(8),
        );
        // Without wrapping the count is the logical line count, however
        // narrow the box (the paragraph truncates instead of folding).
        let clipping = Modal::new(&theme, "t", body);
        assert_eq!(clipping.wrapped_row_count(8), 1);
    }

    #[test]
    fn wrapped_row_count_includes_the_footer_rows() {
        let theme = Theme::default();
        let plain = Modal::new(&theme, "t", vec![Line::from("body")]).wrap(true);
        let footed = plain.clone().footer("hint");
        // Footer adds its spacer + text row to the scroll extent, since
        // both are painted as body rows.
        assert_eq!(
            footed.wrapped_row_count(20),
            plain.wrapped_row_count(20) + 2,
        );
    }

    #[test]
    fn modal_scroll_hides_leading_body_rows() {
        let theme = Theme::default();
        let body = vec![Line::from("first"), Line::from("second")];
        let modal = Modal::new(&theme, "t", body).wrap(true).scroll(1);
        // 3-row box: borders + a single interior row, which after a
        // one-row scroll shows the second line, not the first.
        let text = render_modal(&modal, 12, 3);
        assert!(!text.contains("first"), "scrolled-off row painted:\n{text}");
        assert!(text.contains("second"), "row under scroll missing:\n{text}");
    }

    #[test]
    fn modal_scroll_skips_wrapped_rows_not_logical_lines() {
        let theme = Theme::default();
        // One logical line that wraps to two display rows at the interior
        // width. If scroll skipped logical lines, scroll(1) would jump
        // clean past both halves to "tail"; skipping *display* rows shows
        // the second half of the wrapped line.
        let body = vec![Line::from("alpha bravo"), Line::from("tail")];
        let modal = Modal::new(&theme, "t", body).wrap(true).scroll(1);
        let text = render_modal(&modal, 9, 3);
        assert!(
            text.contains("bravo"),
            "scroll must move one wrapped row, exposing the fold:\n{text}"
        );
        assert!(
            !text.contains("alpha"),
            "first wrapped row painted:\n{text}"
        );
    }

    #[test]
    fn chord_table_aligns_columns_across_sections() {
        let theme = Theme::default();
        let table = KeyChordTable::new(
            &theme,
            vec![
                ChordSection::new(
                    "Prefix bindings (C-a)",
                    vec![ChordRow::new("C-a d", "detach")],
                ),
                ChordSection::new("Global bindings", vec![ChordRow::new("F1", "show-help")]),
            ],
        );
        let modal = Modal::new(&theme, "phux help", table.body_lines());
        let text = render_modal(&modal, 60, 16);
        assert!(text.contains("Prefix bindings (C-a)"), "{text}");
        assert!(text.contains("Global bindings"), "{text}");
        assert!(text.contains("C-a d"), "{text}");
        assert!(text.contains("detach"), "{text}");
        assert!(text.contains("show-help"), "{text}");
    }

    #[test]
    fn chord_table_empty_shows_notice() {
        let theme = Theme::default();
        let table = KeyChordTable::new(&theme, vec![ChordSection::new("Empty", Vec::new())])
            .empty_notice("No keybindings configured.");
        let lines = table.body_lines();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn chord_table_skips_empty_sections() {
        let theme = Theme::default();
        let table = KeyChordTable::new(
            &theme,
            vec![
                ChordSection::new("Has rows", vec![ChordRow::new("a", "act")]),
                ChordSection::new("Empty", Vec::new()),
            ],
        );
        let lines = table.body_lines();
        // Header + one row only; the empty section contributes nothing
        // and no trailing spacer is appended.
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn chord_table_byte_output_is_stable() {
        let theme = Theme::default();
        let table = KeyChordTable::new(
            &theme,
            vec![ChordSection::new(
                "Section",
                vec![
                    ChordRow::new("C-a d", "detach"),
                    ChordRow::new("C-a x", "kill-pane"),
                ],
            )],
        );
        let area = Rect::new(0, 0, 32, 6);
        let mut buf = Buffer::empty(area);
        // Render the lines bare (no modal chrome) so the snapshot pins the
        // table's own alignment.
        let para = Paragraph::new(table.body_lines());
        para.render(area, &mut buf);
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
        let outer = Rect::new(0, 0, 120, 40);
        assert!(!is_compact(outer));
        assert_eq!(centered_panel(outer, 6, 30, 10), centered(outer, 6, 30, 10));
        let r = centered_panel(outer, 6, 30, 10);
        assert!(r.x > outer.x && r.y > outer.y);
        assert!(r.width < outer.width && r.height < outer.height);
    }

    /// A viewport starved on both axes gives the modal the whole screen —
    /// on a 50x14 terminal a 60% box is 30x8, and the six rows of shared
    /// modal chrome leave two rows of actual content.
    #[test]
    fn a_starved_viewport_makes_the_modal_full_bleed() {
        let outer = Rect::new(0, 0, 50, 14);
        assert!(is_compact(outer));
        assert_eq!(centered_panel(outer, 6, 30, 10), outer);
    }

    /// The axes are decided independently: a short, wide viewport wants
    /// full height and a centered width, not a stretched row of two-word
    /// entries.
    #[test]
    fn each_axis_goes_full_bleed_on_its_own() {
        // Wide but short.
        let short = Rect::new(0, 0, 160, 12);
        let r = centered_panel(short, 6, 30, 10);
        assert_eq!((r.y, r.height), (short.y, short.height), "full height");
        assert!(r.width < short.width, "width still floats: {r:?}");

        // Narrow but tall.
        let narrow = Rect::new(0, 0, 40, 60);
        let r = centered_panel(narrow, 6, 30, 10);
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
        let r = centered_panel(content, 6, 30, 10);
        assert_eq!(r, content);
        assert_eq!(r.x, 20, "must not paint over the sidebar strip");
    }

    /// A footer is a list of affordances, so a narrow modal drops whole
    /// ones rather than cutting the last in half.
    #[test]
    fn a_narrow_modal_drops_whole_footer_hints() {
        let theme = Theme::default();
        let modal = Modal::new(&theme, "t", vec![]).footer_hints([
            "Enter select",
            "Esc cancel",
            "type to filter",
        ]);
        assert_eq!(
            modal.footer_line(46).as_deref(),
            Some("Enter select  ·  Esc cancel  ·  type to filter")
        );
        assert_eq!(
            modal.footer_line(45).as_deref(),
            Some("Enter select  ·  Esc cancel")
        );
        assert_eq!(modal.footer_line(26).as_deref(), Some("Enter select"));
        // Below the first hint the footer yields entirely; it never
        // renders a fragment.
        assert_eq!(modal.footer_line(11), None);
        for width in 0..50 {
            let line = modal.footer_line(width).unwrap_or_default();
            assert!(
                line.chars().count() <= width,
                "overran at {width}: {line:?}"
            );
        }
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
