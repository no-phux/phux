//! Evidence-only timing of the unchanged native point reader and reconciler.
//!
//! Run: `cargo bench --locked -p phux-tui --features testkit --bench predict_reconcile`.
//! No sockets, windows, profiler permissions, dependencies, or timing thresholds.
//!
//! Each sample seeds a fresh prediction queue through the supported key API,
//! writes authoritative VT to an in-memory 200x60 terminal, and paints before
//! reconciling (as the focused server-frame path does). Renderer and sink storage
//! are reused. Setup, VT parsing, queue seeding, and result assertions are untimed.
//! `reconcile` excludes paint; `paint+reconcile` includes incremental pane paint
//! into a reused Vec, NOT the whole attach handler, overlays, chrome, or OS I/O.
//! Both include the real reconciler's cloning/queue removal/echo bookkeeping,
//! reader FFI/update/row seeking/string allocation, callback counter increment,
//! error checking, and clock overhead. No replica of the reader is measured.
//!
//! Nonempty confirmed payloads alternate a/b so paint sees changed content.
//! Pending stays blank; mismatch alternates x/y; controls may remain clean.
//! These are synthetic ASCII queue shapes, not observed burst frequencies or
//! keystroke/device latency. The source-derived update/row-step counts printed
//! separately are NOT native instrumentation or evidence of a full-grid copy.

#![allow(
    clippy::expect_used,
    clippy::print_stdout,
    missing_docs,
    reason = "asserted, standalone measurement-reporting benchmark"
)]

#[path = "../../../benchmarks/support.rs"]
mod support;

use std::hint::black_box;
use std::time::{Duration, Instant};

use libghostty_vt::Terminal;
use phux_client_core::predict::{
    PredictionKind, PredictionOutcome, PredictionState, PredictiveConfig, ReconcileStats,
    reconcile_terminal_output_per_cell_at,
};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_tui::attach::render::{ReplicaWalk, TerminalRenderer};
use support::{MEASURED_SAMPLES, WARMUP_SAMPLES, percentile};

const COLS: u16 = 200;
const ROWS: u16 = 60;
const RUNS: usize = 3;
const QUEUED_MS: u64 = 100;
const RECONCILE_MS: u64 = 120;

#[derive(Clone, Copy, Debug)]
enum Case {
    Confirmed(u16),
    Pending,
    Mismatch,
    Empty,
    CursorOnly,
}

impl Case {
    const fn queued(self) -> u16 {
        match self {
            Self::Confirmed(k) => k,
            Self::Pending | Self::Mismatch => 32,
            Self::Empty => 0,
            Self::CursorOnly => 1,
        }
    }

    fn expected(self) -> (ReconcileStats, usize) {
        let k = usize::from(self.queued());
        let (confirmed, pending, contradicted, reads) = match self {
            Self::Confirmed(_) => (k, 0, 0, k),
            Self::Pending => (0, k, 0, 1),
            Self::Mismatch => (0, 0, k, 1),
            Self::Empty => (0, 0, 0, 0),
            Self::CursorOnly => (1, 0, 0, 0),
        };
        (
            ReconcileStats {
                confirmed,
                pending,
                contradicted,
            },
            reads,
        )
    }

    fn payload(self, text: &str, index: usize) -> String {
        match self {
            Self::Confirmed(k) => text.repeat(usize::from(k)),
            Self::Mismatch => ["x", "y"][index % 2].to_owned(),
            Self::CursorOnly => "x\x1b[1D".to_owned(),
            Self::Pending | Self::Empty => String::new(),
        }
    }

    const fn authoritative_col(self) -> u16 {
        match self {
            Self::Confirmed(k) => k,
            Self::Mismatch => 1,
            Self::Pending | Self::Empty | Self::CursorOnly => 0,
        }
    }
}

fn key(key: PhysicalKey, text: Option<&str>) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: text.map(str::to_owned),
        unshifted_codepoint: None,
    }
}

struct Fixture {
    terminal: Terminal<'static, 'static>,
    renderer: TerminalRenderer<'static>,
    sink: Vec<u8>,
    row: u16,
    case: Case,
}

impl Fixture {
    fn new(row: u16, case: Case) -> Self {
        let mut fixture = Self {
            terminal: Terminal::new(COLS, ROWS).expect("in-memory terminal"),
            renderer: TerminalRenderer::new().expect("renderer"),
            sink: Vec::new(),
            row,
            case,
        };
        // Give the cursor-only seed a real known glyph to move over.
        fixture
            .terminal
            .vt_write(format!("\x1b[{};1Hx", row + 1).as_bytes());
        fixture.paint();
        fixture
    }

    fn paint(&mut self) {
        self.renderer
            .render_at(
                ReplicaWalk::for_test(&self.terminal),
                &mut self.sink,
                (0, 0),
                (COLS, ROWS),
            )
            .expect("paint before reconcile");
    }

    fn seed(&mut self, text: &str) -> PredictionState {
        let mut state = PredictionState::new(PredictiveConfig::enabled(), COLS, ROWS);
        // Empty queue must resync a genuinely different estimate without a read.
        let initial_col = if matches!(self.case, Case::Empty) {
            7
        } else {
            0
        };
        state.set_cursor(self.row, initial_col);
        if matches!(self.case, Case::CursorOnly) {
            state.set_cursor(self.row, 1);
            let outcome = state.predict_key_with_grid_at(
                &key(PhysicalKey::ArrowLeft, None),
                QUEUED_MS,
                |r, c| {
                    self.renderer
                        .read_grapheme_at(ReplicaWalk::for_test(&self.terminal), r, c)
                        .expect("arrow seed reads real cell")
                },
            );
            assert_eq!(outcome, PredictionOutcome::Predicted);
            assert_eq!(
                state.pending().next().expect("cursor prediction").kind,
                PredictionKind::CursorLeft
            );
        } else {
            let event = key(PhysicalKey::A, Some(text));
            for _ in 0..self.case.queued() {
                assert_eq!(
                    state.predict_key_at(&event, QUEUED_MS),
                    PredictionOutcome::Predicted
                );
            }
        }
        assert_eq!(state.pending_len(), usize::from(self.case.queued()));
        state
    }

    fn sample(&mut self, index: usize, include_paint: bool) -> Duration {
        let text = ["a", "b"][index % 2];
        let mut state = self.seed(text);
        let payload = self.case.payload(text, index);
        self.terminal
            .vt_write(format!("\x1b[{};1H\x1b[2K{payload}", self.row + 1).as_bytes());
        self.sink.clear();
        let start = if include_paint {
            let start = Instant::now();
            self.paint();
            start
        } else {
            self.paint();
            Instant::now()
        };
        let (row, col) = self
            .renderer
            .last_cursor_local()
            .expect("visible painted cursor");
        let mut reads = 0;
        let summary = reconcile_terminal_output_per_cell_at(
            black_box(&mut state),
            row,
            col,
            RECONCILE_MS,
            |r, c| {
                reads += 1;
                self.renderer
                    .read_grapheme_string_at(ReplicaWalk::for_test(&self.terminal), r, c)
                    .expect("real point reader")
            },
        );
        let elapsed = start.elapsed();
        self.verify(&state, summary, reads);
        black_box(self.sink.len());
        elapsed
    }

    fn verify(&self, state: &PredictionState, summary: ReconcileStats, reads: usize) {
        let (expected, expected_reads) = self.case.expected();
        assert_eq!(summary, expected);
        assert_eq!(reads, expected_reads, "actual reconcile callback count");
        assert_eq!(state.pending_len(), expected.pending);
        assert_eq!(
            self.renderer.last_cursor_local(),
            Some((self.row, self.case.authoritative_col()))
        );
        let cursor_col = if matches!(self.case, Case::Pending) {
            self.case.queued()
        } else {
            self.case.authoritative_col()
        };
        assert_eq!(state.cursor(), (self.row, cursor_col));
        assert_eq!(
            state.echo_confirmed(),
            matches!(self.case, Case::Confirmed(_))
        );
    }
}

fn measure(run: usize, row: u16, case: Case, include_paint: bool) {
    let mut fixture = Fixture::new(row, case);
    for i in 0..WARMUP_SAMPLES {
        black_box(fixture.sample(i, include_paint));
    }
    let mut samples: Vec<_> = (0..MEASURED_SAMPLES)
        .map(|i| fixture.sample(i + WARMUP_SAMPLES, include_paint))
        .collect();
    let (summary, reads) = case.expected();
    let scope = if include_paint {
        "paint+reconcile"
    } else {
        "reconcile"
    };
    println!(
        "run={run} row={row:02} case={case:?} scope={scope} p50={}ns p90={}ns \
         reads/sample={reads} confirmed={} pending={} contradicted={}",
        percentile(&mut samples, 50).as_nanos(),
        percentile(&mut samples, 90).as_nanos(),
        summary.confirmed,
        summary.pending,
        summary.contradicted,
    );
}

fn main() {
    println!(
        "predict_reconcile: 200x60 ASCII, runs={RUNS}, warmup={WARMUP_SAMPLES}, samples={MEASURED_SAMPLES}; assertions on every sample; durations in ns"
    );
    println!(
        "reconcile includes real core/reader + callback counting; paint+reconcile adds pane paint to Vec (not OS I/O or full handler); setup/VT/seed/assertions excluded"
    );
    println!(
        "SOURCE ONLY, not instrumented: reads K => K native updates + K*(row+1) successful row steps; zero-read controls => 0/0; no full-copy/frequency/device-latency claim"
    );
    for run in 1..=RUNS {
        for row in [0, 59] {
            for case in [
                Case::Confirmed(1),
                Case::Confirmed(4),
                Case::Confirmed(32),
                Case::Pending,
                Case::Mismatch,
                Case::Empty,
                Case::CursorOnly,
            ] {
                measure(run, row, case, false);
                measure(run, row, case, true);
            }
        }
    }
}
