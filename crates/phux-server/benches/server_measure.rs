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
use libghostty_vt::snapshot::incremental::{
    CaptureEventKind, CaptureOptions, Error as SnapshotError,
};
use libghostty_vt::{Terminal as GhosttyTerminal, TerminalOptions};
use phux_server::grid::{SCROLLBACK_ALL, SnapshotSynthesizer};

use crate::support::{Corpus, deterministic_line};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CaptureMeasurement {
    pub(crate) ready: Duration,
    pub(crate) full_history: Duration,
    pub(crate) ready_bytes: usize,
    pub(crate) full_history_bytes: usize,
    pub(crate) history_slice_max: Duration,
    pub(crate) history_slice_max_bytes: usize,
    pub(crate) chunks: usize,
    pub(crate) caller_buffer_growths: usize,
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
    let mut terminal = GhosttyTerminal::new(TerminalOptions {
        cols,
        rows,
        max_scrollback,
    })
    .expect("benchmark terminal");

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
    let mut terminal = GhosttyTerminal::new(TerminalOptions {
        cols: 200,
        rows: 60,
        max_scrollback: 50_000,
    })
    .expect("Unicode READY control terminal");
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
    let history_slice_bytes = full.scrollback.len();
    CaptureMeasurement {
        ready: ready_elapsed,
        full_history: full_elapsed,
        ready_bytes: ready.bytes.len(),
        full_history_bytes: full.bytes.len().saturating_add(full.scrollback.len()),
        history_slice_max: if history_slice_bytes == 0 {
            Duration::ZERO
        } else {
            full_elapsed
        },
        history_slice_max_bytes: history_slice_bytes,
        chunks: usize::from(!full.scrollback.is_empty()).saturating_add(1),
        caller_buffer_growths: usize::from(ready.bytes.capacity() != 0)
            .saturating_add(usize::from(full.bytes.capacity() != 0))
            .saturating_add(usize::from(full.scrollback.capacity() != 0)),
        payload_copies: 0,
    }
}

pub(crate) fn native_ready_measurement(
    terminal: &mut GhosttyTerminal<'_, '_>,
) -> CaptureMeasurement {
    // Measure the codec's READY path directly. Server publication preflights
    // and frame staging are covered by the UDS gate below; including a second
    // validation capture here would measure the same codec serialization
    // twice and mislabel that as codec latency.
    let options = CaptureOptions::default();
    let mut buffer = vec![0_u8; options.max_record_bytes];
    let started = Instant::now();
    let mut capture = terminal.capture(options).expect("native capture");
    let mut bytes = 0_usize;
    let mut chunks = 0_usize;
    loop {
        let buffer_ptr = buffer.as_ptr();
        let buffer_len = buffer.len();
        let event = capture.next(&mut buffer).expect("native prefix record");
        debug_assert_eq!(event.record.as_ptr(), buffer_ptr);
        debug_assert!(event.record.len() <= buffer_len);
        bytes = bytes
            .checked_add(event.record.len())
            .expect("prefix byte accounting");
        chunks = chunks.checked_add(1).expect("prefix chunk accounting");
        if matches!(event.kind, CaptureEventKind::Ready { .. }) {
            break;
        }
    }
    let ready = started.elapsed();
    capture.abort().expect("abort after READY");
    CaptureMeasurement {
        ready,
        ready_bytes: bytes,
        chunks,
        caller_buffer_growths: 1,
        payload_copies: 0,
        ..CaptureMeasurement::default()
    }
}

pub(crate) fn native_full_measurement(
    terminal: &mut GhosttyTerminal<'_, '_>,
) -> CaptureMeasurement {
    let started = Instant::now();
    let options = CaptureOptions {
        max_record_bytes: crate::support::HISTORY_PAGE_LIMIT,
        ..CaptureOptions::default()
    };
    let mut capture = terminal.capture(options).expect("native full capture");
    let mut buffer = Vec::new();
    let mut ready = Duration::ZERO;
    let mut ready_bytes = 0_usize;
    let mut full_bytes = 0_usize;
    let mut chunks = 0_usize;
    let mut growths = 0_usize;
    let mut history_slice_max = Duration::ZERO;
    let mut history_slice_max_bytes = 0_usize;
    let mut reached_ready = false;
    let mut copies = 0_usize;
    loop {
        let required = match capture.next(&mut []) {
            Err(SnapshotError::OutOfSpace {
                required_bytes,
                required_rows: 0,
            }) => required_bytes,
            other => panic!("native size probe did not return a byte bound: {other:?}"),
        };
        if required > buffer.capacity() {
            growths = growths.saturating_add(1);
        }
        buffer.resize(required, 0);
        let buffer_ptr = buffer.as_ptr();
        let buffer_len = buffer.len();
        let step_started = Instant::now();
        let event = capture.next(&mut buffer).expect("native full record");
        let step_elapsed = step_started.elapsed();
        let record_len = event.record.len();
        copies = copies.saturating_add(usize::from(
            event.record.as_ptr() != buffer_ptr || record_len > buffer_len,
        ));
        chunks = chunks.saturating_add(1);
        full_bytes = full_bytes
            .checked_add(record_len)
            .expect("full byte accounting");
        if !reached_ready {
            ready_bytes = ready_bytes
                .checked_add(record_len)
                .expect("READY byte accounting");
        }
        match event.kind {
            CaptureEventKind::Ready { .. } => {
                reached_ready = true;
                ready = started.elapsed();
            }
            CaptureEventKind::HistoryBegin { .. } | CaptureEventKind::HistoryPage { .. } => {
                history_slice_max = history_slice_max.max(step_elapsed);
                history_slice_max_bytes = history_slice_max_bytes.max(record_len);
            }
            CaptureEventKind::Finish => break,
            CaptureEventKind::Record => {}
        }
    }
    CaptureMeasurement {
        ready,
        full_history: started.elapsed(),
        ready_bytes,
        full_history_bytes: full_bytes,
        history_slice_max,
        history_slice_max_bytes,
        chunks,
        caller_buffer_growths: growths,
        payload_copies: copies,
    }
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
