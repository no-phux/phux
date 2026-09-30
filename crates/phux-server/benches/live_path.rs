//! Live-path stage benchmark (phux-69pq.5): the state-sync grid tick and the
//! wire copies every PTY chunk pays between the server and a client replica.
//!
//! Two halves, both on the real code:
//!
//! 1. A deterministic table (always printed first): allocations and bytes
//!    requested per stage, from a thread-local counting allocator, across
//!    dirty-row and consumer-count cases. These figures do not depend on
//!    machine load, so they are the numbers to compare before and after a
//!    change.
//! 2. Criterion timings of the same stages, for relative CPU comparisons on
//!    one quiet machine.
//!
//! Stages:
//!
//! - `state-sync`: one `prepare_tick` render plus one `diff_consumer` per
//!   consumer ([`SnapshotSynthesizer::synthesize_tick`]), exactly as the
//!   actor's tick runs it, after `dirty` rows changed.
//! - `wire`: the server writer encoding one `RESOURCE_OUTPUT` per consumer
//!   into its reused batch buffer, then one client framing that frame off
//!   its socket buffer and decoding it (`split_frame` + `freeze` +
//!   `FrameKind::decode`, the `phux-client-runtime` read path).
//!
//! `PHUX_LIVE_PATH_TABLE_ONLY=1` prints the table and skips Criterion.

#![allow(
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::print_stdout,
    missing_docs,
    reason = "measurement binary"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use criterion::{BenchmarkId, Criterion, Throughput};
use libghostty_vt::Terminal as GhosttyTerminal;
use phux_protocol::ResourceId;
use phux_protocol::ids::{BootstrapId, StreamId};
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::framing;
use phux_server::grid::{ConsumerReference, SnapshotSynthesizer};

std::thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static ALLOC_BYTES: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: forwards every call to `System` unchanged; the only addition is
// bumping thread-local `Cell`s, which never touch the returned memory.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        // SAFETY: the caller upholds `alloc`'s layout contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note(new_size);
        // SAFETY: the caller upholds `realloc`'s pointer/layout contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller upholds `dealloc`'s pointer/layout pairing.
        unsafe { System.dealloc(ptr, layout) }
    }
}

fn note(size: usize) {
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        let _ = ALLOC_BYTES.try_with(|n| n.set(n.get() + size));
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Allocations and bytes requested on this thread while `body` runs.
fn counted<T>(body: impl FnOnce() -> T) -> (T, usize, usize) {
    ALLOCS.set(0);
    ALLOC_BYTES.set(0);
    COUNTING.set(true);
    let out = body();
    COUNTING.set(false);
    (out, ALLOCS.get(), ALLOC_BYTES.get())
}

const GEOMETRIES: [(u16, u16); 2] = [(80, 24), (200, 60)];
const CONSUMERS: [usize; 3] = [1, 2, 8];
const PAYLOADS: [usize; 3] = [1024, 16 * 1024, 48 * 1024];

/// Dirty-row cases for a geometry: one row, a quarter, every row.
fn dirty_cases(rows: u16) -> [u16; 3] {
    [1, (rows / 4).max(1), rows]
}

struct Grid {
    terminal: GhosttyTerminal<'static, 'static>,
    synth: SnapshotSynthesizer<'static>,
    references: Vec<ConsumerReference>,
    rows: u16,
    tick: usize,
}

impl Grid {
    fn new((cols, rows): (u16, u16), consumers: usize) -> Self {
        let mut terminal = GhosttyTerminal::new(cols, rows).expect("terminal");
        terminal
            .set_scrollback_max_lines(Some(100))
            .expect("scrollback");
        let mut synth = SnapshotSynthesizer::new().expect("synthesizer");
        let mut references: Vec<_> = (0..consumers).map(|_| ConsumerReference::new()).collect();
        for reference in &mut references {
            synth
                .prime_reference(&terminal, reference)
                .expect("prime reference");
        }
        let mut grid = Self {
            terminal,
            synth,
            references,
            rows,
            tick: 0,
        };
        // Warm the render pool and every scratch buffer to steady capacity.
        for _ in 0..3 {
            grid.dirty(rows);
            grid.tick();
        }
        grid
    }

    /// Rewrite the first `rows` rows with colored text unique to this tick.
    fn dirty(&mut self, rows: u16) {
        self.tick += 1;
        for row in 0..rows.min(self.rows) {
            let fg = 16 + (usize::from(row) * 37 + self.tick) % 216;
            let line = format!(
                "\x1b[{};1H\x1b[38;5;{fg}mrow {row:03} tick {:06} \x1b[1mbold\x1b[0m plain text to fill the line",
                row + 1,
                self.tick,
            );
            self.terminal.vt_write(line.as_bytes());
        }
    }

    fn tick(&mut self) -> usize {
        self.synth
            .synthesize_tick(&self.terminal, &mut self.references)
            .expect("tick")
            .iter()
            .map(|diff| diff.bytes.len())
            .sum()
    }
}

fn output_frame(payload: &Bytes) -> FrameKind {
    FrameKind::ResourceOutput {
        terminal_id: ResourceId::Local { id: 7 },
        stream_id: StreamId::new(1).expect("stream"),
        bootstrap_id: BootstrapId::new(1).expect("generation"),
        seq: 42,
        bytes: payload.clone(),
    }
}

/// The server writer's half: one encode per consumer into reused batches.
fn server_encode(frame: &FrameKind, batches: &mut [BytesMut]) -> usize {
    batches
        .iter_mut()
        .map(|batch| {
            batch.clear();
            frame.encode(batch);
            batch.len()
        })
        .sum()
}

/// One client's read half: frame it off the socket buffer and decode it.
fn client_decode(socket: &mut BytesMut, wire: &[u8]) -> FrameKind {
    socket.clear();
    socket.extend_from_slice(wire);
    let frame = framing::split_frame(socket)
        .expect("framing")
        .expect("one whole frame")
        .freeze();
    FrameKind::decode(&frame).expect("decode").0
}

fn print_table() {
    println!("stage,case,consumers,allocs_per_tick,alloc_bytes_per_tick,out_bytes_per_tick");
    for geometry in GEOMETRIES {
        for dirty in dirty_cases(geometry.1) {
            for consumers in CONSUMERS {
                let mut grid = Grid::new(geometry, consumers);
                grid.dirty(dirty);
                let (out, allocs, bytes) = counted(|| grid.tick());
                println!(
                    "state-sync,{}x{} dirty={dirty},{consumers},{allocs},{bytes},{out}",
                    geometry.0, geometry.1,
                );
            }
        }
    }
    for size in PAYLOADS {
        let payload = Bytes::from(vec![b'x'; size]);
        let frame = output_frame(&payload);
        for consumers in CONSUMERS {
            let mut batches: Vec<_> = (0..consumers)
                .map(|_| BytesMut::with_capacity(64 * 1024))
                .collect();
            let (encoded, allocs, bytes) = counted(|| server_encode(&frame, &mut batches));
            println!("wire-encode,payload={size},{consumers},{allocs},{bytes},{encoded}");
        }
        let mut wire = BytesMut::new();
        frame.encode(&mut wire);
        let mut socket = BytesMut::with_capacity(64 * 1024);
        let (_, allocs, bytes) = counted(|| black_box(client_decode(&mut socket, &wire)));
        println!("client-decode,payload={size},1,{allocs},{bytes},{size}");
    }
}

fn criterion_state_sync(c: &mut Criterion) {
    let mut group = c.benchmark_group("state-sync-tick");
    for geometry in GEOMETRIES {
        for dirty in dirty_cases(geometry.1) {
            for consumers in [1, 8] {
                let mut grid = Grid::new(geometry, consumers);
                group.throughput(Throughput::Elements(1));
                group.bench_function(
                    BenchmarkId::new(
                        format!("{}x{}-dirty{dirty}", geometry.0, geometry.1),
                        consumers,
                    ),
                    |b| {
                        b.iter(|| {
                            grid.dirty(dirty);
                            black_box(grid.tick())
                        });
                    },
                );
            }
        }
    }
    group.finish();
}

fn criterion_wire(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire-copy");
    for size in PAYLOADS {
        let payload = Bytes::from(vec![b'x'; size]);
        let frame = output_frame(&payload);
        let mut batches = vec![BytesMut::with_capacity(64 * 1024)];
        let mut wire = BytesMut::new();
        frame.encode(&mut wire);
        let mut socket = BytesMut::with_capacity(64 * 1024);
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function(BenchmarkId::new("server-encode", size), |b| {
            b.iter(|| black_box(server_encode(&frame, &mut batches)));
        });
        group.bench_function(BenchmarkId::new("client-decode", size), |b| {
            b.iter(|| black_box(client_decode(&mut socket, &wire)));
        });
    }
    group.finish();
}

fn main() {
    print_table();
    if std::env::var_os("PHUX_LIVE_PATH_TABLE_ONLY").is_some() {
        return;
    }
    let mut criterion = Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2))
        .sample_size(30)
        .configure_from_args();
    criterion_state_sync(&mut criterion);
    criterion_wire(&mut criterion);
    criterion.final_summary();
}
