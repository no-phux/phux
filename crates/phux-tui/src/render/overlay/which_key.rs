//! Which-key popup.
//!
//! After the prefix and `which-key-delay-ms` of hesitation, a floating legend
//! of every prefix-table continuation from the live [`KeybindingsCfg`],
//! grouped by what it acts on (pane, tab, session, phux itself). Keys bound
//! to the same action share one row, numeric tab jumps collapse to `0-9`,
//! and a directional family (`h j k l` focus) collapses to one row. Labels
//! are the palette's titles, so a key and its palette row read the same.
//!
//! It is transparent to input ([`RenderOverlay::is_input_passthrough`]): the
//! next key dismisses it and runs as if it had never appeared (Esc also
//! cancels the prefix), so it can never eat or delay a chord.

use phux_config::KeybindingsCfg;
use phux_protocol::input::key::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::widgets::Modal;
use super::{OverlayCommand, RenderOverlay};
use crate::render::Theme;

mod legend;
use legend::{Group, Row, rows_for};
pub use legend::{LegendRow, legend};

/// Which-key popup: prefix-table continuations as grouped `key  action` rows.
#[derive(Debug)]
pub struct WhichKeyOverlay {
    /// Pretty-printed prefix chord as authored in config (e.g. `"C-a"`),
    /// shown in the title so the user sees which pending prefix this is.
    prefix: String,
    /// Rows in display order: by group, then [`ACTION_ORDER`], then label.
    rows: Vec<Row>,
    /// Color slots snapshotted from the active [`Theme`] at construction.
    theme: Theme,
}

impl WhichKeyOverlay {
    /// Build the popup from the live config snapshot, styled with `theme`.
    #[must_use]
    pub fn from_config(cfg: &KeybindingsCfg, theme: &Theme) -> Self {
        Self {
            prefix: cfg.prefix.clone(),
            rows: rows_for(cfg),
            theme: *theme,
        }
    }
}

/// Cells between two columns of the grid.
const COLUMN_GAP: usize = 4;
/// The longest label a cell shows before it is clipped.
const LABEL_MAX: usize = 22;
/// Shown in place of the grid when the prefix table is empty.
const EMPTY_NOTICE: &str = "No prefix bindings configured.";

/// One painted line of a column: a group header, a binding, or the blank
/// line that separates two groups sharing a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cell {
    Header(Group),
    Binding(usize),
    Gap,
    /// The count of bindings the viewport has no room to show.
    Overflow(usize),
}

/// The laid-out legend: columns of cells, and the shared cell widths.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Grid {
    columns: Vec<Vec<Cell>>,
    key_w: usize,
    label_w: usize,
}

impl Grid {
    const fn cell_w(&self) -> usize {
        self.key_w + 2 + self.label_w
    }

    /// Interior width the grid needs.
    const fn width(&self) -> usize {
        let n = self.columns.len();
        n * self.cell_w() + n.saturating_sub(1) * COLUMN_GAP
    }

    fn rows(&self) -> usize {
        self.columns.iter().map(Vec::len).max().unwrap_or(0)
    }
}

/// Flow `groups` (each a header plus its bindings) into columns at most
/// `height` cells tall. With `whole`, a group that does not fit in what is
/// left of a column starts the next one; otherwise a group may continue
/// into the next column, but never leaves its header alone at the bottom.
fn flow(groups: &[(Group, Vec<usize>)], height: usize, whole: bool) -> Vec<Vec<Cell>> {
    let mut columns: Vec<Vec<Cell>> = vec![Vec::new()];
    for (group, rows) in groups {
        let used = columns.last().map_or(0, Vec::len);
        // Below the separating gap: the whole group, or (when it may split)
        // its header and first binding.
        let room = height.saturating_sub(used + 1);
        let need = if whole { 1 + rows.len() } else { 2 };
        let fits = used == 0 || need <= room;
        if !fits {
            columns.push(Vec::new());
        }
        let cells =
            std::iter::once(Cell::Header(*group)).chain(rows.iter().map(|i| Cell::Binding(*i)));
        for (n, cell) in cells.enumerate() {
            let column_len = columns.last().map_or(0, Vec::len);
            if column_len >= height {
                columns.push(Vec::new());
            } else if n == 0 && column_len > 0 {
                columns
                    .last_mut()
                    .into_iter()
                    .for_each(|c| c.push(Cell::Gap));
            }
            columns.last_mut().into_iter().for_each(|c| c.push(cell));
        }
    }
    columns
}

impl WhichKeyOverlay {
    /// The rows of each group, in display order.
    fn groups(&self) -> Vec<(Group, Vec<usize>)> {
        let mut groups: Vec<(Group, Vec<usize>)> = Vec::new();
        for (i, row) in self.rows.iter().enumerate() {
            match groups.last_mut() {
                Some((group, rows)) if *group == row.group => rows.push(i),
                _ => groups.push((row.group, vec![i])),
            }
        }
        groups
    }

    /// The shortest layout whose columns fit the viewport's width: whole
    /// groups per column when the height allows, split groups otherwise.
    fn grid(&self, area: Rect) -> Grid {
        let key_w = self
            .rows
            .iter()
            .map(|row| crate::render::display_width(&row.key_text()))
            .max()
            .unwrap_or(1);
        let label_w = self
            .rows
            .iter()
            .map(|row| crate::render::display_width(&row.label).min(LABEL_MAX))
            .chain(std::iter::once(Group::Session.header().len()))
            .max()
            .unwrap_or(1);
        let mut grid = Grid {
            columns: Vec::new(),
            key_w,
            label_w,
        };
        // Leave the viewport a margin so the panel floats over the work.
        let room = usize::from(area.width)
            .saturating_mul(9)
            .saturating_div(10)
            .saturating_sub(usize::from(2 + super::widgets::MODAL_PAD * 2));
        let max_columns = ((room + COLUMN_GAP) / (grid.cell_w() + COLUMN_GAP)).max(1);
        let max_height = usize::from(area.height.saturating_sub(3)).max(1);
        let groups = self.groups();
        let tallest = groups
            .iter()
            .map(|(_, rows)| 1 + rows.len())
            .max()
            .unwrap_or(1);
        let total: usize = groups.iter().map(|(_, rows)| 2 + rows.len()).sum();
        let shortest = total.div_ceil(max_columns).max(1);
        let fits = |columns: &Vec<Vec<Cell>>| columns.len() <= max_columns;
        grid.columns = (tallest.max(shortest)..=max_height)
            .map(|h| flow(&groups, h, true))
            .find(fits)
            .or_else(|| {
                (shortest..=max_height)
                    .map(|h| flow(&groups, h, false))
                    .find(fits)
            })
            .unwrap_or_else(|| self.truncated(&groups, max_height, max_columns));
        grid
    }

    /// The layout for a viewport too small for every binding: groups flow
    /// at the full height, only the columns that fit the width stay, and
    /// the last cell says how many bindings the rest of the table holds.
    fn truncated(
        &self,
        groups: &[(Group, Vec<usize>)],
        height: usize,
        max_columns: usize,
    ) -> Vec<Vec<Cell>> {
        let mut columns = flow(groups, height, false);
        if columns.len() <= max_columns {
            return columns;
        }
        columns.truncate(max_columns);
        // The note takes the place of the last cell, so a binding that cell
        // held counts among the hidden; a header or gap left stranded above
        // the note goes too, since it would introduce nothing.
        let Some(last_column) = columns.last_mut() else {
            return columns;
        };
        last_column.pop();
        while matches!(last_column.last(), Some(Cell::Header(_) | Cell::Gap)) {
            last_column.pop();
        }
        let shown = columns
            .iter()
            .flatten()
            .filter(|cell| matches!(cell, Cell::Binding(_)))
            .count();
        let hidden = self.rows.len().saturating_sub(shown);
        if let Some(last_column) = columns.last_mut() {
            last_column.push(Cell::Overflow(hidden));
        }
        columns
    }

    fn cell_spans(&self, grid: &Grid, cell: Cell) -> Vec<Span<'static>> {
        let blank = || vec![Span::raw(" ".repeat(grid.cell_w()))];
        match cell {
            Cell::Gap => blank(),
            Cell::Overflow(n) => {
                let note = crate::render::clip_text(&format!("+ {n} more"), grid.cell_w());
                let pad = grid
                    .cell_w()
                    .saturating_sub(crate::render::display_width(&note));
                vec![
                    Span::styled(note, Style::default().fg(self.theme.dim)),
                    Span::raw(" ".repeat(pad)),
                ]
            }
            Cell::Header(group) => {
                let pad = grid.label_w.saturating_sub(group.header().len());
                vec![
                    Span::raw(" ".repeat(grid.key_w + 2)),
                    Span::styled(
                        group.header(),
                        Style::default()
                            .fg(self.theme.section_header)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" ".repeat(pad)),
                ]
            }
            Cell::Binding(i) => {
                let Some(row) = self.rows.get(i) else {
                    return blank();
                };
                let keys = row.key_text();
                let key_pad = grid
                    .key_w
                    .saturating_sub(crate::render::display_width(&keys));
                let label = crate::render::clip_text(&row.label, grid.label_w);
                let label_pad = grid
                    .label_w
                    .saturating_sub(crate::render::display_width(&label));
                vec![
                    Span::raw(" ".repeat(key_pad)),
                    Span::styled(
                        keys,
                        Style::default()
                            .fg(self.theme.chord)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(label, Style::default().fg(self.theme.text)),
                    Span::raw(" ".repeat(label_pad)),
                ]
            }
        }
    }

    fn grid_lines(&self, grid: &Grid) -> Vec<Line<'static>> {
        if self.rows.is_empty() {
            return vec![Line::from(Span::styled(
                EMPTY_NOTICE,
                Style::default().fg(self.theme.dim),
            ))];
        }
        (0..grid.rows())
            .map(|r| {
                let mut spans = Vec::new();
                for (c, column) in grid.columns.iter().enumerate() {
                    if c > 0 {
                        spans.push(Span::raw(" ".repeat(COLUMN_GAP)));
                    }
                    let cell = column.get(r).copied().unwrap_or(Cell::Gap);
                    spans.extend(self.cell_spans(grid, cell));
                }
                Line::from(spans)
            })
            .collect()
    }
}

impl RenderOverlay for WhichKeyOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let modal_area = self.bounds(area).unwrap_or(area);
        let body = self.grid_lines(&self.grid(area));
        Modal::new(&self.theme, self.prefix.clone(), body)
            .footer("", "esc cancel")
            .render_into(modal_area, buf);
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        // Sized to content and anchored to the bottom, above the work the
        // prefix is about to act on. `grid` already fits the content to the
        // viewport, so a cramped screen needs no separate layout.
        let grid = self.grid(area);
        let chrome = 2 + super::widgets::MODAL_PAD * 2;
        let (content_w, content_h) = if self.rows.is_empty() {
            (crate::render::display_width(EMPTY_NOTICE), 1)
        } else {
            (grid.width(), grid.rows())
        };
        let w = u16::try_from(content_w)
            .unwrap_or(u16::MAX)
            .saturating_add(chrome)
            .min(area.width);
        let h = u16::try_from(content_h)
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .min(area.height);
        let x = area.x + (area.width - w) / 2;
        let y = area.y + area.height.saturating_sub(h + 1);
        let rect = Rect::new(x, y, w, h);
        Some(rect)
    }

    fn handle_key(&mut self, _key: &KeyEvent) -> OverlayCommand {
        // Unreachable: the dispatcher pops passthrough overlays before
        // routing. Dismissing keeps the never-consume invariant anyway.
        OverlayCommand::Dismiss
    }

    fn is_input_passthrough(&self) -> bool {
        true
    }

    fn passthrough_escape_cancels_prefix(&self) -> bool {
        true
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests;
