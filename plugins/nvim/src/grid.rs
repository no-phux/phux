//! Every grid Neovim draws, built from its redraw events. With ext_multigrid
//! each window has its own grid; grid 1 holds what's left. Also tracks where
//! windows, floats and the message grid are, and the highlight table.

use std::collections::{BTreeMap, HashMap};

use rmpv::Value;

use crate::rpc::{handle, number};

/// One grapheme cluster; empty for the right half of a wide char.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    text: [u8; Self::MAX],
    len: u8,
    pub hl: u32,
}

impl Cell {
    /// Longer clusters are stored as U+FFFD.
    const MAX: usize = 19;
    pub const BLANK: Self = Self {
        text: *b" \0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
        len: 1,
        hl: 0,
    };

    pub fn new(s: &str, hl: u32) -> Self {
        let s = if s.len() <= Self::MAX { s } else { "\u{fffd}" };
        let mut text = [0; Self::MAX];
        text[..s.len()].copy_from_slice(s.as_bytes());
        Self {
            text,
            len: s.len() as u8,
            hl,
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.text[..self.len as usize]
    }

    pub const fn is_continuation(&self) -> bool {
        self.len == 0
    }
}

#[derive(Debug, Default)]
pub struct Grid {
    pub width: usize,
    pub height: usize,
    pub cells: Vec<Cell>,
    pub cursor: (usize, usize),
}

impl Grid {
    pub fn row(&self, r: usize) -> &[Cell] {
        &self.cells[r * self.width..(r + 1) * self.width]
    }

    /// New size, keeping the cells that still fit.
    fn resize(&mut self, w: usize, h: usize) {
        let mut cells = vec![Cell::BLANK; w * h];
        let keep = w.min(self.width);
        for r in 0..h.min(self.height) {
            cells[r * w..r * w + keep].copy_from_slice(&self.row(r)[..keep]);
        }
        self.cells = cells;
        self.width = w;
        self.height = h;
        self.cursor = (
            self.cursor.0.min(h.saturating_sub(1)),
            self.cursor.1.min(w.saturating_sub(1)),
        );
    }

    /// `grid_line` cells: `[text, hl?, repeat?]`, from `col`. An absent hl
    /// means the previous cell's.
    fn line(&mut self, row: usize, col: usize, cells: &[Value]) {
        if row >= self.height {
            return;
        }
        let w = self.width;
        let dst = &mut self.cells[row * w..(row + 1) * w];
        let mut c = col;
        let mut hl = 0;
        for cell in cells {
            let Some(parts) = cell.as_array() else { return };
            let Some(text) = parts.first().and_then(Value::as_str) else {
                return;
            };
            if let Some(h) = parts.get(1).and_then(Value::as_u64) {
                hl = h as u32;
            }
            let repeat = parts.get(2).and_then(Value::as_u64).unwrap_or(1) as usize;
            let cell = Cell::new(text, hl);
            for _ in 0..repeat {
                if c >= w {
                    return;
                }
                dst[c] = cell;
                c += 1;
            }
        }
    }

    /// Moves rows `top..bot`, columns `left..right`, up by `rows` (down if
    /// negative). `grid_line` refills the rows left behind.
    fn scroll(&mut self, top: usize, bot: usize, left: usize, right: usize, rows: i64) {
        if top > bot || bot > self.height || left > right || right > self.width || rows == 0 {
            return;
        }
        let n = (rows.unsigned_abs() as usize).min(bot - top);
        let copy = |g: &mut Self, from: usize, to: usize| {
            let w = g.width;
            g.cells
                .copy_within(from * w + left..from * w + right, to * w + left);
        };
        if rows > 0 {
            for r in top..bot - n {
                copy(self, r + n, r);
            }
        } else {
            for r in (top + n..bot).rev() {
                copy(self, r - n, r);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// In the tab's split layout.
    Ordinary,
    /// Outside the layout, for the UI to place: these are the panes.
    External,
    /// Floating over another grid.
    Float,
    /// An ordinary window on another tab page.
    Hidden,
}

#[derive(Clone, Copy, Debug)]
pub struct Float {
    pub anchor_grid: u32,
    /// The float's corner at `row`, `col`: south / east.
    pub south: bool,
    pub east: bool,
    pub row: f64,
    pub col: f64,
    pub zindex: u32,
    pub compindex: u32,
    /// Where Neovim put it on grid 1, kept inside the screen.
    pub screen: Option<(i32, i32)>,
}

impl Default for Float {
    fn default() -> Self {
        Self {
            anchor_grid: 1,
            south: false,
            east: false,
            row: 0.0,
            col: 0.0,
            zindex: 50,
            compindex: 0,
            screen: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Win {
    /// Neovim's handle; -1 for the popup menu.
    pub win: i64,
    pub kind: Kind,
    pub float: Float,
}

/// A float placed over a base grid, in that grid's cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    pub grid: u32,
    pub row: i32,
    pub col: i32,
    pub zindex: u32,
    pub compindex: u32,
    /// On the screen (grid 1 or the message grid), not over a window.
    pub on_screen: bool,
    pub on_msg: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Attr {
    pub fg: Option<u32>,
    pub bg: Option<u32>,
    pub sp: Option<u32>,
    pub reverse: bool,
    pub italic: bool,
    pub bold: bool,
    pub strike: bool,
    /// SGR 4 subparameter: 1 line, 2 double, 3 curl, 4 dotted, 5 dashed.
    pub underline: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CursorMode {
    /// 0 block, 1 bar, 2 underline.
    pub shape: u8,
    pub blink: bool,
}

#[derive(Debug, Default)]
pub struct Grids {
    pub grids: HashMap<u32, Grid>,
    /// Windows by grid id, in id order so frames are deterministic.
    pub wins: BTreeMap<u32, Win>,
    pub attrs: HashMap<u32, Attr>,
    pub modes: Vec<CursorMode>,
    pub mode: usize,
    pub busy: bool,
    pub cursor_grid: u32,
    pub msg_grid: Option<u32>,
    pub msg_row: usize,
    /// Bumped by `default_colors_set`, when every pane must repaint.
    pub palette_gen: u32,
}

impl Grids {
    pub fn grid_of(&self, win: i64) -> Option<u32> {
        self.wins
            .iter()
            .find(|(_, w)| w.win == win)
            .map(|(g, _)| *g)
    }

    /// Rows the message grid covers at the bottom of grid 1.
    pub fn msg_rows(&self) -> usize {
        let (Some(id), Some(screen)) = (self.msg_grid, self.grids.get(&1)) else {
            return 0;
        };
        let Some(msg) = self.grids.get(&id) else {
            return 0;
        };
        screen.height.saturating_sub(self.msg_row).min(msg.height)
    }

    /// Where float `id` sits on the grid under it all: floats can float over
    /// floats. Returns the root grid, row and col.
    fn place(&self, id: u32) -> Option<(u32, i32, i32)> {
        let w = self.wins.get(&id)?;
        if w.kind != Kind::Float {
            return None;
        }
        let (mut row, mut col) = (0, 0);
        let mut grid = id;
        let mut f = w.float;
        for _ in 0..8 {
            let g = self.grids.get(&grid)?;
            if f.anchor_grid == 1
                && let Some((r, c)) = f.screen
            {
                return Some((1, row + r, col + c));
            }
            row += f.row.floor() as i32 - if f.south { g.height as i32 } else { 0 };
            col += f.col.floor() as i32 - if f.east { g.width as i32 } else { 0 };
            grid = f.anchor_grid;
            let Some(under) = self.wins.get(&grid) else {
                let shift = if Some(grid) == self.msg_grid {
                    self.msg_row as i32
                } else {
                    0
                };
                return Some((grid, row + shift, col));
            };
            if under.kind != Kind::Float {
                return Some((grid, row, col));
            }
            f = under.float;
        }
        None
    }

    /// The floats over grid `base`, and over the screen too if `screen`,
    /// lowest first.
    pub fn floats_over(&self, base: u32, screen: bool) -> Vec<Placed> {
        let mut out: Vec<Placed> = self
            .wins
            .iter()
            .filter(|(_, w)| w.kind == Kind::Float)
            .filter_map(|(&id, w)| {
                let (root, row, col) = self.place(id)?;
                let on_screen = root == 1 || Some(root) == self.msg_grid;
                (root == base || (screen && on_screen)).then_some(Placed {
                    grid: id,
                    row,
                    col,
                    zindex: w.float.zindex,
                    compindex: w.float.compindex,
                    on_screen,
                    on_msg: Some(root) == self.msg_grid,
                })
            })
            .collect();
        out.sort_by_key(|p| (p.zindex, p.compindex));
        out
    }

    /// Applies a `redraw` notification. True if it ended with `flush`.
    pub fn apply_redraw(&mut self, params: &[Value]) -> bool {
        let mut flushed = false;
        for event in params {
            let Some(e) = event.as_array() else { continue };
            let Some(name) = e.first().and_then(Value::as_str) else {
                continue;
            };
            for args in &e[1..] {
                if let Some(a) = args.as_array() {
                    flushed |= name == "flush";
                    self.apply(name, a);
                }
            }
        }
        flushed
    }

    fn apply(&mut self, name: &str, a: &[Value]) {
        let u = |i: usize| a.get(i).and_then(Value::as_u64).unwrap_or(0) as usize;
        let id = u(0) as u32;
        match name {
            "grid_resize" => self.grids.entry(id).or_default().resize(u(1), u(2)),
            "grid_clear" => {
                if let Some(g) = self.grids.get_mut(&id) {
                    g.cells.fill(Cell::BLANK);
                }
            }
            "grid_line" => {
                if let (Some(g), Some(cells)) =
                    (self.grids.get_mut(&id), a.get(3).and_then(Value::as_array))
                {
                    g.line(u(1), u(2), cells);
                }
            }
            "grid_scroll" => {
                if let Some(g) = self.grids.get_mut(&id) {
                    let rows = a.get(5).and_then(Value::as_i64).unwrap_or(0);
                    g.scroll(u(1), u(2), u(3), u(4), rows);
                }
            }
            "grid_cursor_goto" => {
                self.cursor_grid = id;
                if let Some(g) = self.grids.get_mut(&id) {
                    g.cursor = (u(1), u(2));
                }
            }
            "grid_destroy" => {
                self.grids.remove(&id);
                self.wins.remove(&id);
            }
            "win_pos" | "win_external_pos" => {
                if let Some(win) = a.get(1).and_then(handle) {
                    let kind = if name == "win_pos" {
                        Kind::Ordinary
                    } else {
                        Kind::External
                    };
                    self.wins.insert(
                        id,
                        Win {
                            win,
                            kind,
                            float: Float::default(),
                        },
                    );
                }
            }
            "win_float_pos" => {
                if let (Some(win), Some(float)) = (a.get(1).and_then(handle), parse_float(a)) {
                    self.wins.insert(
                        id,
                        Win {
                            win,
                            kind: Kind::Float,
                            float,
                        },
                    );
                }
            }
            "win_hide" => {
                if let Some(w) = self.wins.get_mut(&id) {
                    w.kind = Kind::Hidden;
                }
            }
            "win_close" => {
                self.wins.remove(&id);
            }
            "msg_set_pos" => {
                self.msg_grid = Some(id);
                self.msg_row = u(1);
            }
            "hl_attr_define" => {
                if let Some(map) = a.get(1).and_then(Value::as_map) {
                    self.attrs.insert(id, parse_attr(map));
                }
            }
            "default_colors_set" => self.palette_gen += 1,
            "mode_info_set" => {
                if let Some(modes) = a.get(1).and_then(Value::as_array) {
                    self.modes = modes.iter().map(parse_mode).collect();
                }
            }
            "mode_change" => self.mode = u(1),
            "busy_start" => self.busy = true,
            "busy_stop" => self.busy = false,
            _ => {}
        }
    }
}

fn get<'a>(map: &'a [(Value, Value)], key: &str) -> Option<&'a Value> {
    map.iter()
        .find(|(k, _)| k.as_str() == Some(key))
        .map(|(_, v)| v)
}

fn parse_float(a: &[Value]) -> Option<Float> {
    let anchor = a.get(2)?.as_str()?.as_bytes();
    Some(Float {
        south: anchor.first() == Some(&b'S'),
        east: anchor.get(1) == Some(&b'E'),
        anchor_grid: a.get(3)?.as_u64()? as u32,
        row: number(a.get(4)?)?,
        col: number(a.get(5)?)?,
        zindex: a.get(7)?.as_u64()? as u32,
        compindex: a.get(8).and_then(Value::as_u64).unwrap_or(0) as u32,
        screen: match (
            a.get(9).and_then(Value::as_i64),
            a.get(10).and_then(Value::as_i64),
        ) {
            (Some(r), Some(c)) => Some((r as i32, c as i32)),
            _ => None,
        },
    })
}

fn parse_attr(map: &[(Value, Value)]) -> Attr {
    let color = |k| get(map, k).and_then(Value::as_u64).map(|c| c as u32);
    let on = |k| get(map, k).and_then(Value::as_bool).unwrap_or(false);
    let underline = [
        ("underline", 1),
        ("underdouble", 2),
        ("undercurl", 3),
        ("underdotted", 4),
        ("underdashed", 5),
    ]
    .into_iter()
    .find(|(k, _)| on(k))
    .map_or(0, |(_, n)| n);
    Attr {
        fg: color("foreground"),
        bg: color("background"),
        sp: color("special"),
        reverse: on("reverse"),
        italic: on("italic"),
        bold: on("bold"),
        strike: on("strikethrough"),
        underline,
    }
}

fn parse_mode(v: &Value) -> CursorMode {
    let Some(map) = v.as_map() else {
        return CursorMode::default();
    };
    let shape = match get(map, "cursor_shape").and_then(Value::as_str) {
        Some("vertical") => 1,
        Some("horizontal") => 2,
        _ => 0,
    };
    let blink = get(map, "blinkon").and_then(Value::as_i64).unwrap_or(0) > 0
        && get(map, "blinkoff").and_then(Value::as_i64).unwrap_or(0) > 0;
    CursorMode { shape, blink }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(name: &str, args: Vec<Value>) -> Value {
        Value::Array(vec![name.into(), Value::Array(args)])
    }
    fn ints(v: &[i64]) -> Vec<Value> {
        v.iter().map(|&n| n.into()).collect()
    }
    fn line(grid: i64, row: i64, col: i64, cells: Vec<Value>) -> Value {
        ev(
            "grid_line",
            vec![
                grid.into(),
                row.into(),
                col.into(),
                Value::Array(cells),
                false.into(),
            ],
        )
    }
    fn cell(v: Vec<Value>) -> Value {
        Value::Array(v)
    }
    fn text(g: &Grid, r: usize) -> String {
        g.row(r)
            .iter()
            .map(|c| String::from_utf8_lossy(c.bytes()).into_owned())
            .collect()
    }
    fn float(
        grid: i64,
        anchor: &str,
        on: i64,
        at: (f64, f64),
        screen: Option<(i64, i64)>,
    ) -> Value {
        let z: i64 = if on == 3 { 40 } else { 50 };
        let mut a = vec![
            grid.into(),
            1000.into(),
            anchor.into(),
            on.into(),
            at.0.into(),
            at.1.into(),
            true.into(),
            z.into(),
        ];
        if let Some((r, c)) = screen {
            a.extend([0.into(), r.into(), c.into()]);
        }
        ev("win_float_pos", a)
    }

    #[test]
    fn lines_carry_highlights_and_repeats() {
        let mut gs = Grids::default();
        let flushed = gs.apply_redraw(&[
            ev("grid_resize", ints(&[1, 6, 2])),
            line(
                1,
                0,
                0,
                vec![
                    cell(vec!["h".into(), 3.into()]),
                    cell(vec!["e".into()]),
                    cell(vec!["l".into(), 3.into(), 2.into()]),
                    cell(vec!["o".into(), 4.into()]),
                ],
            ),
            ev("grid_cursor_goto", ints(&[1, 1, 2])),
            ev("flush", vec![]),
        ]);
        assert!(flushed);
        let g = &gs.grids[&1];
        assert_eq!(text(g, 0), "hello ");
        let hls: Vec<u32> = g.row(0).iter().map(|c| c.hl).collect();
        assert_eq!(hls, [3, 3, 3, 3, 4, 0]);
        assert_eq!(g.cursor, (1, 2));
    }

    #[test]
    fn scrolling_moves_rows_both_ways() {
        let mut gs = Grids::default();
        gs.apply_redraw(&[
            ev("grid_resize", ints(&[1, 1, 3])),
            line(1, 0, 0, vec![cell(vec!["a".into(), 0.into()])]),
            line(1, 1, 0, vec![cell(vec!["b".into(), 0.into()])]),
            line(1, 2, 0, vec![cell(vec!["c".into(), 0.into()])]),
        ]);
        gs.apply_redraw(&[ev("grid_scroll", ints(&[1, 0, 3, 0, 1, 1, 0]))]);
        assert_eq!(text(&gs.grids[&1], 0), "b");
        assert_eq!(text(&gs.grids[&1], 1), "c");
        gs.apply_redraw(&[ev("grid_scroll", ints(&[1, 0, 3, 0, 1, -1, 0]))]);
        assert_eq!(text(&gs.grids[&1], 2), "c");
    }

    #[test]
    fn long_clusters_become_replacement_chars() {
        let c = Cell::new(&"x".repeat(40), 1);
        assert_eq!(c.bytes(), "\u{fffd}".as_bytes());
        assert!(Cell::new("", 0).is_continuation());
    }

    #[test]
    fn floats_stack_over_their_base_and_the_screen() {
        let mut gs = Grids::default();
        gs.apply_redraw(&[
            ev("grid_resize", ints(&[2, 20, 10])),
            ev("win_external_pos", ints(&[2, 1000])),
            ev("grid_resize", ints(&[3, 4, 2])),
            ev("grid_resize", ints(&[4, 3, 1])),
            ev("grid_resize", ints(&[5, 5, 2])),
            float(3, "SE", 2, (6.5, 10.0), None),
            float(4, "NW", 3, (1.0, 1.0), Some((0, 0))),
            float(5, "NW", 1, (30.0, 30.0), Some((7, 2))),
        ]);
        let over = gs.floats_over(2, false);
        let placed: Vec<_> = over.iter().map(|p| (p.grid, p.row, p.col)).collect();
        assert_eq!(placed, [(4, 5, 7), (3, 4, 6)]);
        let all = gs.floats_over(2, true);
        assert_eq!(all.len(), 3);
        assert_eq!(
            (all[2].grid, all[2].row, all[2].col, all[2].on_screen),
            (5, 7, 2, true)
        );
    }

    #[test]
    fn closing_forgets_windows_and_grids() {
        let mut gs = Grids::default();
        gs.apply_redraw(&[
            ev("grid_resize", ints(&[2, 3, 2])),
            ev("win_external_pos", ints(&[2, 1000])),
        ]);
        assert_eq!(gs.grid_of(1000), Some(2));
        gs.apply_redraw(&[ev("win_close", ints(&[2])), ev("grid_destroy", ints(&[2]))]);
        assert_eq!(gs.grid_of(1000), None);
        assert!(gs.grids.is_empty());
    }
}
