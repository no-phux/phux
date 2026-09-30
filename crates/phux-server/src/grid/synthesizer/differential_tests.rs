//! Differential gate for the incremental state-sync tick (phux-69pq.13).
//!
//! Two terminals receive the same seeded random VT workload. Terminal `a` is
//! ticked by one long-lived synthesizer, so its ticks take the incremental
//! path (dirty rows only), with foreign walks and metadata reads mixed in the
//! way the actor interleaves them. Terminal `b` is ticked by a synthesizer
//! whose pool is rebuilt before every tick, so each of its ticks is a full
//! render of a fresh render state: the pre-incremental output. Every tick
//! must match byte for byte, as must every consumer's emit-once and
//! loss-tolerant diff.

use libghostty_vt::terminal::ScrollViewport;

use super::*;

/// Seeds per geometry; each run is a few hundred operations.
const SEEDS: u64 = 24;
const STEPS: usize = 220;
const GEOMETRIES: [(u16, u16); 3] = [(12, 5), (40, 10), (80, 24)];

/// xorshift64*: deterministic, dependency-free, good enough to pick ops.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform-ish in `0..n` (`n > 0`).
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).unwrap_or(1)).unwrap_or(0)
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

/// Text fragments: ASCII, wide glyphs (spacer tails), a combining mark, an
/// emoji with a skin-tone modifier, and a hyperlink.
const TEXT: [&str; 9] = [
    "hello",
    "the quick brown fox",
    "\u{6f22}\u{5b57}",
    "e\u{301}",
    "\u{1f600}x",
    "\u{1f44d}\u{1f3fd}",
    "\x1b]8;;https://example.invalid\x1b\\link\x1b]8;;\x1b\\",
    "\ttab",
    "back\x08\x08ck",
];

const SGR: [&str; 14] = [
    "0",
    "1",
    "3",
    "4",
    "7",
    "9",
    "22",
    "31",
    "42",
    "53",
    "38;5;196",
    "48;5;22",
    "38;2;1;2;3",
    "48;2;200;100;50",
];

/// One random workload step, as bytes for both terminals.
fn random_vt(rng: &mut Rng, cols: u16, rows: u16) -> Vec<u8> {
    let row = rng.below(usize::from(rows)) + 1;
    let col = rng.below(usize::from(cols)) + 1;
    let n = rng.below(4) + 1;
    let text = *rng.pick(&TEXT);
    let sgr = *rng.pick(&SGR);
    let top = rng.below(usize::from(rows)) + 1;
    let bottom = (top + rng.below(usize::from(rows))).min(usize::from(rows));
    let seq = match rng.below(26) {
        // Plain and styled text, often wrapping past the right margin.
        0..=4 => format!(
            "\x1b[{sgr}m{text}{}",
            "w".repeat(rng.below(usize::from(cols) * 2))
        ),
        5 => format!("\x1b[{row};{col}H{text}"),
        // Style-only: the same text rewritten in a new pen.
        6 => format!("\x1b[{row};1H\x1b[{sgr}mSAME TEXT\x1b[0m"),
        // Cursor-only changes.
        7 => format!("\x1b[{row};{col}H"),
        8 => (*rng.pick(&[
            "\x1b[?25l",
            "\x1b[?25h",
            "\x1b[?12h",
            "\x1b[?12l",
            "\x1b[3 q",
        ]))
        .to_owned(),
        // Full-screen scroll: every row shifts (and the viewport moves).
        9 => format!("\x1b[{rows};1H{}", "\r\nscrolled".repeat(n)),
        // Region scroll, both directions, then reset the margins.
        10 => format!(
            "\x1b[{top};{bottom}r\x1b[{bottom};1H{}\x1b[r",
            "\nregion".repeat(n)
        ),
        11 => format!("\x1b[{top};{bottom}r\x1b[{n}S\x1b[r"),
        12 => format!("\x1b[{top};{bottom}r\x1b[{n}T\x1b[r"),
        13 => format!("\x1b[{row};1H\x1b[{n}L"),
        14 => format!("\x1b[{row};1H\x1b[{n}M"),
        15 => format!("\x1b[{row};{col}H\x1b[{sgr}m\x1b[{n}@\x1b[{n}P\x1b[{n}X"),
        // Clears, including a colored erase (background color erase).
        16 => format!(
            "\x1b[{row};{col}H\x1b[{sgr}m{}",
            rng.pick(&[
                "\x1b[J", "\x1b[1J", "\x1b[2J", "\x1b[3J", "\x1b[K", "\x1b[1K", "\x1b[2K"
            ])
        ),
        // Alternate screen switches.
        17 => (*rng.pick(&[
            "\x1b[?1049h",
            "\x1b[?1049l",
            "\x1b[?47h",
            "\x1b[?47l",
            "\x1b[?1047h",
            "\x1b[?1047l",
        ]))
        .to_owned(),
        // Palette and default colors: terminal-wide changes.
        18 => (*rng.pick(&[
            "\x1b]4;196;rgb:12/34/56\x1b\\",
            "\x1b]4;22;rgb:ff/00/ff\x1b\\",
            "\x1b]104\x1b\\",
            "\x1b]10;rgb:aa/bb/cc\x1b\\",
            "\x1b]11;rgb:01/02/03\x1b\\",
            "\x1b[?5h",
            "\x1b[?5l",
        ]))
        .to_owned(),
        19 => "\x1b#8".to_owned(),
        20 => "\x1bc".to_owned(),
        // Mode bits that ride in the epilogue.
        21 => (*rng.pick(&[
            "\x1b[?2004h",
            "\x1b[?2004l",
            "\x1b[?1000h",
            "\x1b[?1006h",
            "\x1b[?1004h",
        ]))
        .to_owned(),
        22 => (*rng.pick(&["\x1b[?7l", "\x1b[?7h", "\x1b[4h", "\x1b[4l"])).to_owned(),
        23 => format!("\x1b7\x1b[{top};{bottom}r\x1b[?6h\x1b[{n};1Horigin\x1b[?6l\x1b[r\x1b8"),
        24 => format!("\x1b[{row};{col}H\x1b[{sgr}m{text}\x1b[0m\r\n"),
        _ => format!("\x1b[{row};{col}H\x1bD\x1bM\x1bE"),
    };
    seq.into_bytes()
}

/// One consumer on both sides: emit-once reference and loss-tolerant base.
struct Consumer {
    a: ConsumerReference,
    b: ConsumerReference,
    base_a: ConsumerReference,
    base_b: ConsumerReference,
}

struct Pair {
    a: GhosttyTerminal<'static, 'static>,
    b: GhosttyTerminal<'static, 'static>,
    synth_a: SnapshotSynthesizer<'static>,
    synth_b: SnapshotSynthesizer<'static>,
    consumers: Vec<Consumer>,
    /// Ticks on `a` that rendered some but not all rows.
    partial_ticks: usize,
    /// Ticks on `a` that rendered nothing.
    clean_ticks: usize,
}

impl Pair {
    fn new(cols: u16, rows: u16) -> Self {
        let term = || {
            let mut t = GhosttyTerminal::new(cols, rows).expect("Terminal::new");
            t.set_scrollback_max_lines(Some(50)).expect("scrollback");
            t
        };
        Self {
            a: term(),
            b: term(),
            synth_a: SnapshotSynthesizer::new().expect("synth a"),
            synth_b: SnapshotSynthesizer::new().expect("synth b"),
            consumers: Vec::new(),
            partial_ticks: 0,
            clean_ticks: 0,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        self.a.vt_write(bytes);
        self.b.vt_write(bytes);
    }

    /// A consumer joins mid-stream: primed (the attach path) or unprimed
    /// (first diff must repaint every row).
    fn join(&mut self, primed: bool) {
        let mut consumer = Consumer {
            a: ConsumerReference::new(),
            b: ConsumerReference::new(),
            base_a: ConsumerReference::new(),
            base_b: ConsumerReference::new(),
        };
        if primed {
            self.synth_a
                .prime_reference(&self.a, &mut consumer.a)
                .expect("prime a");
            self.synth_b
                .prime_reference(&self.b, &mut consumer.b)
                .expect("prime b");
        }
        self.consumers.push(consumer);
    }

    fn tick(&mut self, ctx: &str, ack: bool) {
        let tick_a = self.synth_a.prepare_tick(&self.a).expect("tick a");
        // The full renderer: a rebuilt pool renders every row fresh.
        self.synth_b.note_foreign_walk();
        let tick_b = self.synth_b.prepare_tick(&self.b).expect("tick b");
        assert_eq!(tick_a, tick_b, "{ctx}: tick dims / cursor-mode");
        let (cols, rows_n, live_cm) = tick_a;
        match self.synth_a.last_rendered_rows {
            0 => self.clean_ticks += 1,
            n if n < usize::from(rows_n) => self.partial_ticks += 1,
            _ => {}
        }
        for (row, (a, b)) in self
            .synth_a
            .tick_rows
            .iter()
            .zip(&self.synth_b.tick_rows)
            .enumerate()
        {
            assert_eq!(
                String::from_utf8_lossy(a),
                String::from_utf8_lossy(b),
                "{ctx}: row {row} body diverged from the full render",
            );
        }
        assert_eq!(
            self.synth_a.tick_rows, self.synth_b.tick_rows,
            "{ctx}: rows"
        );
        assert_eq!(
            self.synth_a.tick_epilogue, self.synth_b.tick_epilogue,
            "{ctx}"
        );
        assert_eq!(
            self.synth_a.tick_screen_toggle, self.synth_b.tick_screen_toggle,
            "{ctx}"
        );
        for (i, consumer) in self.consumers.iter_mut().enumerate() {
            let da = self
                .synth_a
                .diff_consumer(cols, rows_n, live_cm, &mut consumer.a);
            let db = self
                .synth_b
                .diff_consumer(cols, rows_n, live_cm, &mut consumer.b);
            assert_eq!(da.bytes, db.bytes, "{ctx}: consumer {i} emit-once diff");
            let la = self
                .synth_a
                .diff_against_base(cols, rows_n, live_cm, &consumer.base_a);
            let lb = self
                .synth_b
                .diff_against_base(cols, rows_n, live_cm, &consumer.base_b);
            assert_eq!(la, lb, "{ctx}: consumer {i} loss-tolerant diff");
            if ack {
                consumer.base_a = self.synth_a.snapshot_tick_reference(cols, rows_n, live_cm);
                consumer.base_b = self.synth_b.snapshot_tick_reference(cols, rows_n, live_cm);
            }
        }
    }
}

fn run(seed: u64, (cols, rows): (u16, u16)) -> (usize, usize) {
    let mut rng = Rng::new(seed ^ (u64::from(cols) << 32) ^ u64::from(rows));
    let mut pair = Pair::new(cols, rows);
    let (mut cur_cols, mut cur_rows) = (cols, rows);
    pair.join(true);
    for step in 0..STEPS {
        let ctx = format!("seed {seed} {cols}x{rows} step {step}");
        match rng.below(100) {
            0..=2 => {
                cur_cols = u16::try_from(rng.below(usize::from(cols) * 2) + 2).unwrap_or(cols);
                cur_rows = u16::try_from(rng.below(usize::from(rows) * 2) + 1).unwrap_or(rows);
                pair.a.resize(cur_cols, cur_rows, 8, 16).expect("resize a");
                pair.b.resize(cur_cols, cur_rows, 8, 16).expect("resize b");
            }
            3..=4 => {
                let scroll = *rng.pick(&[
                    ScrollViewport::Delta(-3),
                    ScrollViewport::Delta(2),
                    ScrollViewport::Top,
                    ScrollViewport::Bottom,
                ]);
                pair.a.scroll_viewport(scroll);
                pair.b.scroll_viewport(scroll);
            }
            5 => pair.join(rng.chance(50)),
            // The actor's other readers of `a`, between ticks: the
            // metadata reader shares the pool, the rest walk fresh states.
            6..=7 => drop(pair.synth_a.metadata_snapshot(&pair.a).expect("metadata")),
            8 => drop(pair.synth_a.screen_state(&pair.a, 0).expect("screen_state")),
            9 => drop(pair.synth_a.synthesize(&pair.a).expect("synthesize")),
            _ => {
                let bytes = random_vt(&mut rng, cur_cols, cur_rows);
                pair.write(&bytes);
            }
        }
        // Tick often, and sometimes twice in a row (a clean tick).
        if rng.chance(45) {
            let ack = rng.chance(50);
            pair.tick(&ctx, ack);
            if rng.chance(10) {
                pair.tick(&format!("{ctx} (again)"), ack);
            }
        }
    }
    pair.tick(&format!("seed {seed} {cols}x{rows} final"), true);
    (pair.partial_ticks, pair.clean_ticks)
}

#[test]
fn incremental_tick_matches_full_render_under_random_workloads() {
    let mut partial = 0;
    let mut clean = 0;
    for geometry in GEOMETRIES {
        for seed in 0..SEEDS {
            let (p, c) = run(seed, geometry);
            partial += p;
            clean += c;
        }
    }
    // The gate is only meaningful if the incremental paths actually ran.
    assert!(partial > 100, "only {partial} partial ticks exercised");
    assert!(clean > 20, "only {clean} clean ticks exercised");
}
