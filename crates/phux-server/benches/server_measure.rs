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
use phux_server::grid::{SCROLLBACK_ALL, SnapshotSynthesizer};
use std::io::Cursor;

use crate::support::{Corpus, deterministic_line};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CaptureMeasurement {
    /// Time until the synchronous encoder releases the canonical terminal.
    pub(crate) capture_blocking: Duration,
    /// End-to-end codec time to capture, decode, and authenticate READY.
    pub(crate) ready: Duration,
    /// Time to decode and authenticate the already-available prefix through READY.
    pub(crate) decode_ready: Duration,
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
    let mut terminal = {
        let mut terminal = GhosttyTerminal::new(cols, rows).expect("benchmark terminal");
        terminal
            .set_scrollback_max_lines(Some(max_scrollback))
            .expect("benchmark terminal");
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
    let history_slice_bytes = full.scrollback.len();
    CaptureMeasurement {
        capture_blocking: ready_elapsed,
        ready: ready_elapsed,
        decode_ready: Duration::ZERO,
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
    let started = Instant::now();
    let mut encoded = Vec::new();
    terminal
        .encode_snapshot(&mut encoded)
        .expect("encode snapshot");
    let capture_elapsed = started.elapsed();
    let mut reader = Cursor::new(encoded.as_slice());
    let decoder = libghostty_vt::snapshot::Decoder::new(&mut reader).expect("decoder");
    let decode_started = Instant::now();
    drop(decoder.ready().expect("ready"));
    let decode_ready = decode_started.elapsed();
    let ready_bytes = usize::try_from(reader.position()).expect("offset");
    CaptureMeasurement {
        capture_blocking: capture_elapsed,
        ready: capture_elapsed.saturating_add(decode_ready),
        decode_ready,
        ready_bytes,
        chunks: 1,
        caller_buffer_growths: 1,
        payload_copies: 0,
        ..CaptureMeasurement::default()
    }
}

pub(crate) fn native_full_measurement(
    terminal: &mut GhosttyTerminal<'_, '_>,
) -> CaptureMeasurement {
    let started = Instant::now();
    let mut encoded = Vec::new();
    terminal
        .encode_snapshot(&mut encoded)
        .expect("encode snapshot");
    let encode_elapsed = started.elapsed();
    let mut ready_reader = Cursor::new(encoded.as_slice());
    let ready_decoder =
        libghostty_vt::snapshot::Decoder::new(&mut ready_reader).expect("ready decoder");
    drop(ready_decoder.ready().expect("ready"));
    let ready_bytes = usize::try_from(ready_reader.position()).expect("offset");
    let mut reader = Cursor::new(encoded.as_slice());
    let decoder = libghostty_vt::snapshot::Decoder::new(&mut reader).expect("decoder");
    let decode_started = Instant::now();
    let mut decoder = decoder.ready().expect("ready");
    let decode_ready = decode_started.elapsed();
    let mut previous_offset = ready_bytes;
    let mut history_slice_max = Duration::ZERO;
    let mut history_slice_max_bytes = 0_usize;
    let full_decode_started = Instant::now();
    loop {
        let page_started = Instant::now();
        let Some(progress) = decoder.next().expect("decode history page") else {
            break;
        };
        history_slice_max = history_slice_max.max(page_started.elapsed());
        let offset = progress
            .as_decoder()
            .source_offset()
            .expect("history source offset");
        history_slice_max_bytes =
            history_slice_max_bytes.max(offset.saturating_sub(previous_offset));
        previous_offset = offset;
    }
    let full_decode_elapsed = full_decode_started.elapsed();
    CaptureMeasurement {
        capture_blocking: encode_elapsed,
        ready: encode_elapsed.saturating_add(decode_ready),
        decode_ready,
        full_history: full_decode_elapsed,
        ready_bytes,
        full_history_bytes: encoded.len(),
        history_slice_max,
        history_slice_max_bytes,
        chunks: 1,
        caller_buffer_growths: 1,
        payload_copies: 0,
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
