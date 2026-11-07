//! Instruction counts for the Terminal engine's hot paths, guarded by the
//! `perf-icount` job with `phux-server`'s `icount` bench. Each function is one
//! callgrind region; setup runs outside it. Scenarios:
//!
//! - VT apply: libghostty parses a mixed text and SGR stream.
//! - PTY batch handoff: the burst copy the actor performs for one wakeup.

#![allow(clippy::expect_used, missing_docs, reason = "callgrind harness")]

use std::hint::black_box;

use bytes::Bytes;
use iai_callgrind::{library_benchmark, library_benchmark_group, main};
use libghostty_vt::Terminal as GhosttyTerminal;
use phux_terminal::actor::icount_join_pty_burst;

const VT_COLS: u16 = 80;
const VT_ROWS: u16 = 24;
const PTY_CHUNKS: usize = 47;
const PTY_CHUNK_BYTES: usize = 1024;

struct VtReady {
    terminal: GhosttyTerminal<'static, 'static>,
    bytes: Vec<u8>,
}

fn vt_bytes() -> Vec<u8> {
    let line = b"\x1b[38;5;2mthe quick brown fox jumps over the lazy dog\x1b[0m\r\n";
    let mut out = Vec::with_capacity(8192);
    while out.len() < 8192 {
        out.extend_from_slice(line);
    }
    out
}

fn setup_vt() -> VtReady {
    let mut terminal = GhosttyTerminal::new(VT_COLS, VT_ROWS).expect("terminal");
    // One warm write so the measured call is steady-state parsing.
    terminal.vt_write(b"\x1b[H");
    VtReady {
        terminal,
        bytes: vt_bytes(),
    }
}

fn setup_pty() -> Vec<Bytes> {
    (0..PTY_CHUNKS)
        .map(|index| Bytes::from(vec![u8::try_from(index).unwrap_or(0); PTY_CHUNK_BYTES]))
        .collect()
}

#[library_benchmark]
#[bench::apply(setup = setup_vt)]
fn vt_apply(mut ready: VtReady) -> usize {
    ready.terminal.vt_write(black_box(&ready.bytes));
    black_box(&ready.terminal);
    ready.bytes.len()
}

#[library_benchmark]
#[bench::join(setup = setup_pty)]
fn pty_batch_handoff(chunks: Vec<Bytes>) -> usize {
    black_box(icount_join_pty_burst(black_box(chunks)).len())
}

library_benchmark_group!(
    name = engine_hot_path;
    benchmarks = vt_apply, pty_batch_handoff
);

main!(library_benchmark_groups = engine_hot_path);
