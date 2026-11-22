//! What one pane's terminal shows: a window's grid, the floats over it and,
//! in the focused pane, the screen's floats and the message grid over the
//! bottom rows. Each frame composes every cell and sends what differs from
//! the last, so nothing needs to replay scrolls or track dirty rows.

use std::fmt::Write as _;

use crate::grid::{Attr, Cell, Grid, Grids, Placed};

/// A float as drawn, in terminal cells; mouse events on it go to its grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub grid: u32,
    pub row: i32,
    pub col: i32,
    pub width: usize,
    pub height: usize,
}

impl Rect {
    pub const fn contains(&self, row: i32, col: i32) -> bool {
        row >= self.row
            && row < self.row + self.height as i32
            && col >= self.col
            && col < self.col + self.width as i32
    }
}

#[derive(Debug)]
pub struct View {
    pub cols: usize,
    pub rows: usize,
    shown: Vec<Cell>,
    full: bool,
    cursor: Option<(usize, usize, u8)>,
    palette_gen: u32,
    /// The floats drawn last frame, lowest first.
    pub rects: Vec<Rect>,
}

impl View {
    pub const fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            shown: Vec::new(),
            full: true,
            cursor: None,
            palette_gen: 0,
            rects: Vec::new(),
        }
    }

    /// The window: the pane less its message line, never empty.
    pub fn window_size(&self) -> (usize, usize) {
        (self.cols.max(1), self.rows.saturating_sub(1).max(1))
    }

    pub const fn resize(&mut self, cols: usize, rows: usize) {
        if cols != self.cols || rows != self.rows {
            self.cols = cols;
            self.rows = rows;
            self.full = true;
        }
    }

    /// Terminal bytes for this frame; empty when nothing changed.
    pub fn frame(&mut self, grids: &Grids, grid_id: Option<u32>, focused: bool) -> Vec<u8> {
        let (next, msg) = self.compose(grids, grid_id, focused);
        let cursor = if focused && !grids.busy {
            self.cursor_at(grids, grid_id, msg)
        } else {
            None
        };
        if grids.palette_gen != self.palette_gen {
            self.palette_gen = grids.palette_gen;
            self.full = true;
        }
        let full = self.full || self.shown.len() != next.len();
        let mut bytes = Vec::new();
        if full {
            bytes.extend_from_slice(b"\x1b[0m\x1b[2J");
        }
        for r in 0..self.rows {
            let row = &next[r * self.cols..(r + 1) * self.cols];
            let old = (!full).then(|| &self.shown[r * self.cols..(r + 1) * self.cols]);
            draw_row(row, old, r, grids, &mut bytes);
        }
        if bytes.is_empty() && cursor == self.cursor {
            return Vec::new();
        }
        let mut framed = b"\x1b[?2026h\x1b[?25l".to_vec();
        framed.append(&mut bytes);
        framed.extend_from_slice(b"\x1b[0m");
        if let Some((row, col, style)) = cursor {
            framed.extend_from_slice(
                format!("\x1b[{style} q\x1b[{};{}H\x1b[?25h", row + 1, col + 1).as_bytes(),
            );
        }
        framed.extend_from_slice(b"\x1b[?2026l");
        self.shown = next;
        self.full = false;
        self.cursor = cursor;
        framed
    }

    /// Every cell of the frame, and the message rows shown.
    fn compose(
        &mut self,
        grids: &Grids,
        grid_id: Option<u32>,
        focused: bool,
    ) -> (Vec<Cell>, usize) {
        let (cols, rows) = (self.cols, self.rows);
        let mut cells = vec![Cell::BLANK; cols * rows];
        let total = if focused { grids.msg_rows() } else { 0 };
        let msg_n = total.min(rows);
        let msg_top = rows - msg_n;
        if let Some(g) = grid_id.and_then(|id| grids.grids.get(&id)) {
            for r in 0..g.height.min(msg_top) {
                blit(&mut cells, cols, r, 0, g.row(r));
            }
        }
        self.rects.clear();
        if let Some(id) = grid_id {
            let screen_rows = grids.grids.get(&1).map_or(0, |g| g.height);
            for p in grids.floats_over(id, focused) {
                let Some(f) = grids.grids.get(&p.grid) else {
                    continue;
                };
                let rect = self.fit(p, f, screen_rows);
                for r in 0..f.height {
                    let row = rect.row + r as i32;
                    if row >= 0 && (row as usize) < msg_top {
                        blit(&mut cells, cols, row as usize, rect.col, f.row(r));
                    }
                }
                self.rects.push(rect);
            }
        }
        let msg = grids.msg_grid.filter(|_| msg_n > 0);
        if let Some(m) = msg.and_then(|id| grids.grids.get(&id)) {
            for i in 0..msg_n {
                blit(&mut cells, cols, msg_top + i, 0, m.row(total - msg_n + i));
            }
        }
        (cells, msg_n)
    }

    /// Screen floats kept inside the pane where they fit. Message floats
    /// keep to the bottom, as the messages do.
    fn fit(&self, p: Placed, f: &Grid, screen_rows: usize) -> Rect {
        let (mut row, mut col) = (p.row, p.col);
        if p.on_screen {
            if p.on_msg {
                row -= screen_rows as i32 - self.rows as i32;
            }
            row = fit_into(row, f.height, self.rows);
            col = fit_into(col, f.width, self.cols);
        }
        Rect {
            grid: p.grid,
            row,
            col,
            width: f.width,
            height: f.height,
        }
    }

    fn cursor_at(
        &self,
        grids: &Grids,
        grid_id: Option<u32>,
        msg_n: usize,
    ) -> Option<(usize, usize, u8)> {
        let at = grids.cursor_grid;
        let g = grids.grids.get(&at)?;
        let (r, c) = (g.cursor.0 as i32, g.cursor.1 as i32);
        let msg_top = self.rows - msg_n;
        let pos = if Some(at) == grids.msg_grid {
            let skipped = grids.msg_rows().saturating_sub(msg_n) as i32;
            (msg_top as i32 + r - skipped, c)
        } else if Some(at) == grid_id {
            (r, c)
        } else {
            let f = self.rects.iter().find(|f| f.grid == at)?;
            (f.row + r, f.col + c)
        };
        if pos.0 < 0 || pos.1 < 0 || pos.0 as usize >= self.rows || pos.1 as usize >= self.cols {
            return None;
        }
        let mode = grids.modes.get(grids.mode).copied().unwrap_or_default();
        // DECSCUSR: 1/2 block, 3/4 underline, 5/6 bar; odd blinks.
        let steady = match mode.shape {
            1 => 6,
            2 => 4,
            _ => 2,
        };
        Some((
            pos.0 as usize,
            pos.1 as usize,
            steady - u8::from(mode.blink),
        ))
    }
}

/// `pos` moved back inside `0..limit` if `len` fits there.
fn fit_into(pos: i32, len: usize, limit: usize) -> i32 {
    let (len, limit) = (len as i32, limit as i32);
    if len > limit {
        pos.min(0).max(limit - len)
    } else {
        pos.clamp(0, limit - len)
    }
}

/// Copies `src` into row `row` at `col`, clipped, repairing any wide char it
/// cuts in half.
fn blit(cells: &mut [Cell], cols: usize, row: usize, col: i32, src: &[Cell]) {
    let line = &mut cells[row * cols..(row + 1) * cols];
    let start = col.max(0) as usize;
    let skip = (-col).max(0) as usize;
    if skip >= src.len() || start >= cols {
        return;
    }
    let n = (src.len() - skip).min(cols - start);
    if start > 0 && line[start].is_continuation() {
        line[start - 1] = Cell::new(" ", line[start - 1].hl);
    }
    line[start..start + n].copy_from_slice(&src[skip..skip + n]);
    if line[start].is_continuation() {
        line[start] = Cell::new(" ", line[start].hl);
    }
    if let Some(after) = line.get_mut(start + n)
        && after.is_continuation()
    {
        *after = Cell::new(" ", after.hl);
    }
}

/// Draws the changed span of a row. A span never starts on the right half
/// of a wide char: the terminal can only write the whole of it.
fn draw_row(row: &[Cell], old: Option<&[Cell]>, r: usize, grids: &Grids, out: &mut Vec<u8>) {
    let differs = |c: usize| old.is_none_or(|o| o[c] != row[c]);
    let Some(mut first) = (0..row.len()).find(|&c| differs(c)) else {
        return;
    };
    let last = (0..row.len()).rfind(|&c| differs(c)).unwrap_or(first);
    while first > 0 && row[first].is_continuation() {
        first -= 1;
    }
    out.extend_from_slice(format!("\x1b[{};{}H", r + 1, first + 1).as_bytes());
    let mut hl = None;
    for cell in &row[first..=last] {
        if cell.is_continuation() {
            continue;
        }
        if hl != Some(cell.hl) {
            hl = Some(cell.hl);
            let attr = grids.attrs.get(&cell.hl).copied().unwrap_or_default();
            out.extend_from_slice(sgr(&attr).as_bytes());
        }
        out.extend_from_slice(cell.bytes());
    }
}

fn sgr(a: &Attr) -> String {
    let mut s = String::from("\x1b[0");
    let rgb = |c: u32| ((c >> 16) & 255, (c >> 8) & 255, c & 255);
    for (on, code) in [
        (a.bold, "1"),
        (a.italic, "3"),
        (a.reverse, "7"),
        (a.strike, "9"),
    ] {
        if on {
            s.push(';');
            s.push_str(code);
        }
    }
    if a.underline > 0 {
        let _ = write!(s, ";4:{}", a.underline);
    }
    if let Some(c) = a.fg {
        let (r, g, b) = rgb(c);
        let _ = write!(s, ";38;2;{r};{g};{b}");
    }
    if let Some(c) = a.bg {
        let (r, g, b) = rgb(c);
        let _ = write!(s, ";48;2;{r};{g};{b}");
    }
    if let Some(c) = a.sp.filter(|_| a.underline > 0) {
        let (r, g, b) = rgb(c);
        let _ = write!(s, ";58:2::{r}:{g}:{b}");
    }
    s.push('m');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{Float, Kind, Win};

    fn grid(text: &[&str]) -> Grid {
        let width = text[0].chars().count();
        let cells = text
            .iter()
            .flat_map(|l| l.chars().map(|c| Cell::new(&c.to_string(), 0)))
            .collect();
        Grid {
            width,
            height: text.len(),
            cells,
            cursor: (0, 0),
        }
    }
    fn text(b: &[u8]) -> String {
        String::from_utf8_lossy(b).into_owned()
    }

    #[test]
    fn only_changes_are_sent() {
        let mut gs = Grids::default();
        gs.grids.insert(2, grid(&["abcd", "efgh"]));
        let mut v = View::new(4, 3);
        assert!(text(&v.frame(&gs, Some(2), false)).contains("abcd"));
        assert!(v.frame(&gs, Some(2), false).is_empty());
        gs.grids.get_mut(&2).unwrap().cells[5] = Cell::new("X", 0);
        let f = text(&v.frame(&gs, Some(2), false));
        assert!(
            f.contains("\x1b[2;2H") && f.contains('X') && !f.contains("abcd"),
            "{f:?}"
        );
    }

    #[test]
    fn floats_cover_the_window_and_are_targets() {
        let mut gs = Grids::default();
        gs.grids.insert(2, grid(&["aaaa", "bbbb"]));
        gs.grids.insert(3, grid(&["F"]));
        let float = Float {
            anchor_grid: 2,
            row: 1.0,
            col: 2.0,
            ..Float::default()
        };
        gs.wins.insert(
            3,
            Win {
                win: 1001,
                kind: Kind::Float,
                float,
            },
        );
        let mut v = View::new(4, 3);
        let f = text(&v.frame(&gs, Some(2), false));
        assert!(f.contains("bbFb"), "{f:?}");
        assert!(v.rects[0].contains(1, 2) && !v.rects[0].contains(0, 2));
    }

    #[test]
    fn messages_show_only_in_the_focused_pane() {
        let mut gs = Grids::default();
        gs.grids.insert(1, grid(&["    ", "    ", "    "]));
        gs.grids.insert(2, grid(&["aaaa", "bbbb"]));
        gs.grids.insert(9, grid(&["msg!"]));
        gs.msg_grid = Some(9);
        gs.msg_row = 2;
        assert!(text(&View::new(4, 3).frame(&gs, Some(2), true)).contains("msg!"));
        assert!(!text(&View::new(4, 3).frame(&gs, Some(2), false)).contains("msg!"));
    }

    #[test]
    fn the_cursor_shows_only_when_focused() {
        let mut gs = Grids {
            cursor_grid: 2,
            ..Grids::default()
        };
        gs.grids.insert(
            2,
            Grid {
                cursor: (1, 2),
                ..grid(&["aaaa", "bbbb"])
            },
        );
        let on = text(&View::new(4, 3).frame(&gs, Some(2), true));
        assert!(on.contains("\x1b[2;3H\x1b[?25h"), "{on:?}");
        assert!(!text(&View::new(4, 3).frame(&gs, Some(2), false)).contains("\x1b[?25h"));
    }

    #[test]
    fn blitting_over_half_a_wide_char_blanks_it() {
        let mut line = vec![Cell::new("界", 0), Cell::new("", 0), Cell::new("x", 0)];
        blit(&mut line, 3, 0, 1, &[Cell::new("y", 0)]);
        assert_eq!(line[0].bytes(), b" ");
        assert_eq!(line[1].bytes(), b"y");
    }

    #[test]
    fn a_span_never_starts_on_a_continuation() {
        let gs = Grids::default();
        let new = [Cell::new("界", 1), Cell::new("", 1)];
        let old = [Cell::new("界", 1), Cell::new("x", 1)];
        let mut out = Vec::new();
        draw_row(&new, Some(&old), 0, &gs, &mut out);
        assert!(text(&out).contains("\x1b[1;1H"));
    }

    #[test]
    fn sgr_includes_styles_and_colours() {
        let a = Attr {
            fg: Some(0x11_22_33),
            bold: true,
            underline: 3,
            sp: Some(0xff_00_00),
            ..Attr::default()
        };
        assert_eq!(sgr(&a), "\x1b[0;1;4:3;38;2;17;34;51;58:2::255:0:0m");
        assert_eq!(sgr(&Attr::default()), "\x1b[0m");
    }

    #[test]
    fn fitting_keeps_floats_inside() {
        assert_eq!(fit_into(10, 3, 5), 2);
        assert_eq!(fit_into(-2, 3, 5), 0);
        assert_eq!(fit_into(0, 8, 5), 0);
        assert_eq!(fit_into(-9, 8, 5), -3);
    }
}
