//! `OutputMode::StateSync` (ADR-0018 / ADR-0042) on bare terminals: a
//! consumer fed per-tick reference-grid deltas ends on the same grid (styles
//! included) as a pass-through consumer fed the raw PTY bytes, and a runaway
//! repaint between ticks coalesces into one delta sized by the visible change.

use libghostty_vt::Terminal as GhosttyTerminal;
use libghostty_vt::render::{CellIterator, RenderState, RowIterator};
use libghostty_vt::screen::CellWide;
use phux_server::grid::{ConsumerReference, SnapshotSynthesizer};

fn fresh(cols: u16, rows: u16) -> GhosttyTerminal<'static, 'static> {
    let mut terminal = GhosttyTerminal::new(cols, rows).unwrap();
    terminal.set_scrollback_max_lines(Some(100)).unwrap();
    terminal
}

/// Right-trimmed viewport rows, wide-cell tails skipped.
fn render_grid(t: &GhosttyTerminal<'_, '_>) -> Vec<String> {
    let mut rs = RenderState::new().unwrap();
    let snap = rs.update(t).unwrap();
    let rows_n = usize::from(snap.rows().unwrap());
    let mut rows = RowIterator::new().unwrap();
    let mut cells = CellIterator::new().unwrap();
    let mut row_iter = rows.update(&snap).unwrap();
    let mut grid = Vec::with_capacity(rows_n);
    while let Some(row) = row_iter.next() {
        if grid.len() >= rows_n {
            break;
        }
        let mut line = String::new();
        let mut cell_iter = cells.update(row).unwrap();
        while let Some(cell) = cell_iter.next() {
            if matches!(
                cell.raw_cell().unwrap().wide().unwrap(),
                CellWide::SpacerTail
            ) {
                continue;
            }
            let g = cell.graphemes().unwrap();
            if g.is_empty() {
                line.push(' ');
            } else {
                line.extend(g);
            }
        }
        grid.push(line.trim_end().to_owned());
    }
    grid
}

fn full_snapshot(t: &GhosttyTerminal<'_, '_>) -> Vec<u8> {
    SnapshotSynthesizer::new()
        .unwrap()
        .synthesize(t)
        .unwrap()
        .bytes
}

/// A canonical terminal plus a state-sync mirror driven one tick at a time.
struct Ticker {
    canonical: GhosttyTerminal<'static, 'static>,
    mirror: GhosttyTerminal<'static, 'static>,
    synth: SnapshotSynthesizer<'static>,
    reference: ConsumerReference,
}

impl Ticker {
    fn new(cols: u16, rows: u16) -> Self {
        let canonical = fresh(cols, rows);
        let mut synth = SnapshotSynthesizer::new().unwrap();
        let mut reference = ConsumerReference::new();
        synth.prime_reference(&canonical, &mut reference).unwrap();
        Self {
            canonical,
            mirror: fresh(cols, rows),
            synth,
            reference,
        }
    }

    /// Synthesize and apply one tick's delta; returns its size.
    fn tick(&mut self) -> usize {
        let delta = self
            .synth
            .synthesize_against_reference(&self.canonical, &mut self.reference)
            .unwrap()
            .bytes;
        self.mirror.vt_write(&delta);
        delta.len()
    }
}

#[test]
#[allow(clippy::too_many_lines, reason = "one table of VT feature cases")]
fn state_sync_converges_with_pass_through() {
    let cases: [(&str, u16, u16, &[&[u8]]); 7] = [
        (
            "text",
            20,
            5,
            &[b"first line", b"\r\nsecond line", b"\r\nthird"],
        ),
        (
            "sgr",
            30,
            4,
            &[
                b"\x1b[31mRED\x1b[0m normal ",
                b"\x1b[1;32mBOLDGREEN\x1b[0m",
                b"\r\n\x1b[38;2;10;20;30mtruecolor\x1b[0m tail",
            ],
        ),
        (
            "cursor",
            20,
            5,
            &[
                b"\x1b[2;3Habc",
                b"\x1b[1;1HTOP",
                b"\x1b[5;1Hbottom",
                b"\rXX",
            ],
        ),
        (
            "scroll region",
            16,
            6,
            &[
                b"top-fixed\r\n",
                b"\x1b[2;4r",
                b"\x1b[2;1Hline-a\r\nline-b\r\nline-c",
                b"\r\nline-d",
                b"\r\nline-e",
            ],
        ),
        (
            "alt screen",
            20,
            5,
            &[
                b"primary content",
                b"\x1b[?1049h",
                b"alt-screen body",
                b"\r\nmore alt",
                b"\x1b[?1049l",
            ],
        ),
        (
            "wide",
            20,
            4,
            &[
                "日本語テスト".as_bytes(),
                b"\r\n",
                "emoji \u{1f600}\u{1f680} end".as_bytes(),
            ],
        ),
        (
            "mixed",
            24,
            6,
            &[
                b"\x1b[33mstatus\x1b[0m ",
                "\u{1f4bb} 日本".as_bytes(),
                b"\x1b[3;1H\x1b[4;1r",
                b"\r\nscroll-1\r\nscroll-2\r\nscroll-3",
                b"\x1b[?1049h",
                b"\x1b[1;1Halt \x1b[31mred\x1b[0m",
                b"\x1b[?1049l",
                b"\x1b[6;1Hfinal-row",
            ],
        ),
    ];
    for (name, cols, rows, chunks) in cases {
        let mut ticker = Ticker::new(cols, rows);
        let mut passthrough = fresh(cols, rows);
        for chunk in chunks {
            ticker.canonical.vt_write(chunk);
            passthrough.vt_write(chunk);
            ticker.tick();
        }
        let grid = render_grid(&ticker.mirror);
        assert_eq!(
            grid,
            render_grid(&passthrough),
            "{name}: state-sync vs pass-through"
        );
        assert_eq!(
            grid,
            render_grid(&ticker.canonical),
            "{name}: state-sync vs canonical"
        );
        assert_eq!(
            full_snapshot(&ticker.mirror),
            full_snapshot(&passthrough),
            "{name}: grids must agree including SGR/cursor/modes"
        );
    }
}

#[test]
fn runaway_repaints_coalesce_into_bounded_deltas() {
    // One tick over 5000 spinner repaints: the delta is tiny next to the raw
    // volume, and the final state lands.
    let mut ticker = Ticker::new(40, 5);
    let mut raw_volume = 0;
    for i in 0..5000 {
        let paint = format!("\x1b[1;1Hframe {i:06} \x1b[7m{}\x1b[0m", "#".repeat(10));
        raw_volume += paint.len();
        ticker.canonical.vt_write(paint.as_bytes());
    }
    ticker
        .canonical
        .vt_write(b"\x1b[1;1HDONE                    \x1b[2;1Hresult: ok");
    let delta = ticker.tick();
    assert_eq!(render_grid(&ticker.mirror), render_grid(&ticker.canonical));
    assert!(render_grid(&ticker.mirror)[0].starts_with("DONE"));
    assert!(delta * 20 < raw_volume, "delta {delta} vs raw {raw_volume}");

    // Across many ticks each delta stays row-bounded and the mirror converges.
    let mut ticker = Ticker::new(40, 4);
    for tick in 0..10 {
        for i in 0..500 {
            ticker
                .canonical
                .vt_write(format!("\x1b[1;1Htick {tick} frame {i:04}").as_bytes());
        }
        let delta = ticker.tick();
        assert!(
            delta < 400,
            "per-tick delta must stay row-bounded; got {delta}"
        );
    }
    assert_eq!(render_grid(&ticker.mirror), render_grid(&ticker.canonical));
}
