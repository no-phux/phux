#![allow(
    dead_code,
    clippy::expect_used,
    clippy::missing_panics_doc,
    clippy::panic,
    reason = "shared by benchmark and deterministic gate"
)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "shared source is included as a private module by multiple benchmark/test crate roots"
)]

use std::hint::black_box;
use std::time::{Duration, Instant};

use bytes::Bytes;
use libghostty_vt::Terminal as GhosttyTerminal;
use phux_client_core::engine::ghostty::snapshot::{EncodedSnapshot, SnapshotCaptureLimits};
use phux_terminal::grid::{SCROLLBACK_ALL, SnapshotSynthesizer};

use crate::support::{Corpus, deterministic_line};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CaptureMeasurement {
    pub(crate) protocol_ready: Duration,
    pub(crate) full_history: Duration,
    pub(crate) engine_ready_bytes: usize,
    pub(crate) protocol_ready_bytes: usize,
    pub(crate) engine_history_bytes: usize,
    pub(crate) full_history_bytes: usize,
    pub(crate) chunks: usize,
    pub(crate) history_pages: usize,
    pub(crate) history_step_max: Duration,
    pub(crate) history_page_max_bytes: usize,
    /// Logical payload materializations, excluding allocator reallocations.
    pub(crate) payload_copies: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FanoutMeasurement {
    pub(crate) elapsed: Duration,
    pub(crate) delivered_bytes: usize,
    pub(crate) payload_copies: usize,
    pub(crate) peak_retained_bytes: usize,
}

pub(crate) fn build_terminal(corpus: Corpus) -> GhosttyTerminal<'static, 'static> {
    let (cols, rows) = corpus.geometry();
    let max_scrollback = corpus.history_lines().max(1_000);
    let mut terminal = {
        let mut terminal = GhosttyTerminal::new(cols, rows).expect("benchmark terminal");
        terminal
            .set_scrollback_max_lines(Some(max_scrollback))
            .expect("benchmark terminal");
        terminal
            .set_scrollback_max_bytes(Some(usize::MAX))
            .expect("benchmark scrollback byte budget");
        terminal
    };

    match corpus {
        Corpus::Shell80x24 => {
            terminal.vt_write(b"$ printf 'ready\\n'\r\nready\r\n$ ");
            terminal.vt_write(b"\x1b[1;32mbranch\x1b[0m feat/negotiated-libghostty-codec\r\n");
            terminal.vt_write("wide: 東京 🦀 combining: e\u{301}\r\n".as_bytes());
        }
        Corpus::Tui200x60 => {
            terminal.vt_write(b"\x1b[?1049h\x1b[2J\x1b[H");
            for row in 0..60_u16 {
                let color = 16 + (u32::from(row) * 37 % 216);
                let line = format!(
                    "\x1b[{};1H\x1b[38;5;{}m{:03} {:<170}\x1b[0m",
                    row + 1,
                    color,
                    row,
                    deterministic_line(usize::from(row)).trim_end(),
                );
                terminal.vt_write(line.as_bytes());
            }
            terminal.vt_write(b"\x1b[30;70H\x1b[7m ACTIVE \x1b[0m\x1b[?25l");
        }
        Corpus::Unicode50k => {
            for index in 0..50_000 {
                let line = deterministic_line(index);
                terminal.vt_write(line.as_bytes());
            }
        }
    }
    terminal
}
pub(crate) fn build_unicode_ready_control() -> GhosttyTerminal<'static, 'static> {
    let mut terminal = {
        let mut terminal = GhosttyTerminal::new(200, 60).expect("Unicode READY control terminal");
        terminal
            .set_scrollback_max_lines(Some(50_000))
            .expect("Unicode READY control terminal");
        terminal
            .set_scrollback_max_bytes(Some(usize::MAX))
            .expect("Unicode READY control bytes");
        terminal
    };
    for index in 49_940..50_000 {
        terminal.vt_write(deterministic_line(index).as_bytes());
    }
    terminal
}

pub(crate) fn synthesized_measurement(terminal: &GhosttyTerminal<'_, '_>) -> CaptureMeasurement {
    let synth = SnapshotSynthesizer::new().expect("snapshot synthesizer");
    let ready_start = Instant::now();
    let ready = synth
        .synthesize(terminal)
        .expect("synthesized READY snapshot");
    let ready_elapsed = ready_start.elapsed();

    let full_start = Instant::now();
    let full = synth
        .synthesize_with_scrollback(terminal, Some(SCROLLBACK_ALL))
        .expect("synthesized full-history snapshot");
    let full_elapsed = full_start.elapsed();
    CaptureMeasurement {
        protocol_ready: ready_elapsed,
        full_history: full_elapsed,
        engine_ready_bytes: ready.bytes.len(),
        protocol_ready_bytes: ready.bytes.len(),
        engine_history_bytes: full.scrollback.len(),
        full_history_bytes: full.bytes.len().saturating_add(full.scrollback.len()),
        chunks: usize::from(!full.scrollback.is_empty()).saturating_add(1),
        history_pages: usize::from(!full.scrollback.is_empty()),
        history_step_max: full_elapsed,
        history_page_max_bytes: full.scrollback.len(),
        payload_copies: usize::from(!ready.bytes.is_empty())
            .saturating_add(usize::from(!full.scrollback.is_empty())),
    }
}

/// Measure synchronous stock capture and zero-copy history unit publication.
pub(crate) fn native_progressive_measurement(
    terminal: &mut GhosttyTerminal<'_, '_>,
) -> CaptureMeasurement {
    let record_bytes = usize::try_from(phux_protocol::DEFAULT_HISTORY_PAGE_BYTES)
        .expect("default history page bound");
    let chunk_bytes =
        usize::try_from(phux_protocol::DEFAULT_BOOTSTRAP_CHUNK_BYTES).expect("default chunk bound");
    let started = Instant::now();
    let capture = EncodedSnapshot::capture(
        terminal,
        SnapshotCaptureLimits {
            max_bytes: phux_terminal_api::native_state::MAX_NATIVE_PREFIX_BYTES,
            max_record_bytes: record_bytes,
            max_records: 100_000,
            max_rows: 100_000,
        },
    )
    .expect("stock capture");
    let protocol_ready = started.elapsed();
    let history_step_max = history_publication_max(&capture);
    let protocol_ready_bytes = capture.prefix_records.last().expect("READY prefix").end;
    let engine_history_bytes = capture.bytes.len() - protocol_ready_bytes;
    let history_pages = capture.history.iter().filter(|unit| !unit.finish).count();
    let history_page_max_bytes = capture
        .history
        .iter()
        .filter(|unit| !unit.finish)
        .map(|unit| unit.range.len())
        .max()
        .unwrap_or(0);
    let chunks = capture
        .prefix_records
        .iter()
        .map(|range| range.len().div_ceil(chunk_bytes))
        .chain(
            capture
                .history
                .iter()
                .map(|unit| unit.range.len().div_ceil(chunk_bytes)),
        )
        .sum();
    CaptureMeasurement {
        protocol_ready,
        full_history: started.elapsed(),
        engine_ready_bytes: protocol_ready_bytes,
        protocol_ready_bytes,
        engine_history_bytes,
        full_history_bytes: protocol_ready_bytes.saturating_add(engine_history_bytes),
        chunks,
        history_pages,
        // All encoding/indexing precedes READY. This measures only the later
        // publication of a shared byte slice, not a fictitious encoding step.
        history_step_max,
        history_page_max_bytes,
        // One stock-to-Rust materialization; indexed units share those bytes.
        payload_copies: 1,
    }
}

fn history_publication_max(capture: &EncodedSnapshot) -> Duration {
    let mut slowest = Duration::ZERO;
    for unit in &capture.history {
        let started = Instant::now();
        drop(black_box(capture.bytes.slice(unit.range.clone())));
        slowest = slowest.max(started.elapsed());
    }
    slowest
}

pub(crate) fn fanout_measurement(
    clients: usize,
    iterations: usize,
    payload: &Bytes,
) -> FanoutMeasurement {
    assert!([1, 2, 8].contains(&clients), "fixed fanout client count");
    let (sender, _) = tokio::sync::broadcast::channel::<Bytes>(iterations.max(1));
    let mut receivers: Vec<_> = (0..clients).map(|_| sender.subscribe()).collect();
    let source_ptr = payload.as_ptr();
    let started = Instant::now();
    for _ in 0..iterations {
        sender
            .send(Bytes::clone(payload))
            .expect("fanout has subscribers");
    }
    let publish_elapsed = started.elapsed();
    let mut delivered = 0_usize;
    let mut copies = 0_usize;
    for receiver in &mut receivers {
        for _ in 0..iterations {
            let received = receiver.try_recv().expect("bounded receiver is current");
            delivered = delivered
                .checked_add(received.len())
                .expect("fanout byte accounting");
            copies = copies.saturating_add(usize::from(received.as_ptr() != source_ptr));
            black_box(received);
        }
    }
    FanoutMeasurement {
        elapsed: publish_elapsed,
        delivered_bytes: delivered,
        payload_copies: copies,
        peak_retained_bytes: payload
            .len()
            .saturating_add(iterations.saturating_mul(std::mem::size_of::<Bytes>())),
    }
}

pub(crate) const fn retained_budget_holds(
    active_terminal_bytes: usize,
    configured_cache_bytes: usize,
    chunk_bytes: usize,
    observed_peak_bytes: usize,
) -> bool {
    observed_peak_bytes
        <= active_terminal_bytes
            .saturating_add(configured_cache_bytes)
            .saturating_add(chunk_bytes.saturating_mul(2))
}
