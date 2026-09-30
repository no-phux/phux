//! An output flood through a real server into the C ABI's connected lane,
//! measured the way Cockpit consumes it: one `phux_client_poll` per display
//! tick. Prints how many output frames the kernel applied against how many
//! owner-thread apply batches and grid publications they cost, plus what the
//! polling thread allocated.
//!
//! A measurement, not a gate: its figures depend on scheduling, so it is
//! ignored by default. Run it with
//!
//! ```text
//! cargo nextest run -p phux-client-ffi --test connected_flood --run-ignored all --no-capture
//! ```
//!
//! `PHUX_FLOOD_LINES` (default 65536, ~4.5 MiB of VT) sizes the flood and
//! `PHUX_FLOOD_TICK_MS` (default 16) the poll interval.

#![allow(clippy::expect_used, reason = "test assertions")]
#![allow(clippy::unwrap_used, reason = "test assertions")]
#![allow(clippy::panic, reason = "test assertions")]
#![allow(
    clippy::print_stdout,
    reason = "the figures are this measurement's output"
)]
#![allow(clippy::cast_precision_loss, reason = "ratios for a printed report")]
#![allow(
    clippy::future_not_send,
    reason = "the C client is owning-thread-only by contract; this future never leaves its thread"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use phux_client_ffi::{
    ABI_VERSION, PhuxAttachOptions, PhuxBytes, PhuxCatalogTerminal, PhuxClient, PhuxClientOptions,
    PhuxClientResult, PhuxClientState, PhuxConnectOptions, PhuxResourceId, PhuxTerminalGridView,
    PhuxWorkspaceInfo, phux_client_catalog_terminal_get, phux_client_connect, phux_client_free,
    phux_client_perf_json, phux_client_poll, phux_client_queue_attach, phux_client_send_paste,
    phux_client_state, phux_client_terminal_grid, phux_client_workspace_info,
};
use phux_server_testkit::{run_local, spawn_server_with_seed_cmd};
use tempfile::TempDir;

const DEADLINE: Duration = Duration::from_secs(120);

/// Counts allocations made on a thread while it is armed: here, only the
/// polling thread inside `phux_client_poll`.
struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: defers to the system allocator; counting has no effect on layout.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.with(Cell::get) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.with(Cell::get) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        }
        // SAFETY: forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn env_or(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

const fn span(text: &str) -> PhuxBytes {
    PhuxBytes {
        data: text.as_ptr(),
        len: text.len(),
    }
}

struct Poller {
    client: *mut PhuxClient,
    polls: u64,
    slowest: Duration,
}

impl Poller {
    fn poll(&mut self) {
        let start = Instant::now();
        ARMED.with(|armed| armed.set(true));
        // SAFETY: a live client, on its owning thread.
        let result = unsafe { phux_client_poll(self.client) };
        ARMED.with(|armed| armed.set(false));
        self.slowest = self.slowest.max(start.elapsed());
        self.polls += 1;
        assert_eq!(result, PhuxClientResult::Ok, "poll");
    }

    async fn until(
        &mut self,
        what: &str,
        tick: Duration,
        mut ready: impl FnMut(*mut PhuxClient) -> bool,
    ) {
        let start = Instant::now();
        while !ready(self.client) {
            assert!(start.elapsed() < DEADLINE, "timed out waiting for {what}");
            self.poll();
            tokio::time::sleep(tick).await;
        }
    }
}

fn counters(client: *mut PhuxClient) -> [u64; 3] {
    let mut out = PhuxBytes::default();
    // SAFETY: a live client and a writable output.
    assert_eq!(
        unsafe { phux_client_perf_json(client, &raw mut out) },
        PhuxClientResult::Ok
    );
    // SAFETY: borrowed from the live client until its next mutable call.
    let json = unsafe { std::slice::from_raw_parts(out.data, out.len) };
    let report: serde_json::Value = serde_json::from_slice(json).expect("perf json");
    let metric = |name: &str| {
        report["metrics"]
            .as_array()
            .expect("metrics")
            .iter()
            .find(|metric| metric["name"] == name)
            .and_then(|metric| metric["value"].as_u64())
            .unwrap_or_else(|| panic!("{name} missing"))
    };
    [
        metric("kernel.frames"),
        metric("runtime.apply_batches"),
        metric("runtime.publish"),
    ]
}

fn grid_text(client: *mut PhuxClient, terminal: &PhuxResourceId) -> String {
    let mut grid = PhuxTerminalGridView::default();
    // SAFETY: client, ID and initialized output are live and disjoint.
    let result = unsafe { phux_client_terminal_grid(client, terminal, &raw mut grid) };
    if result == PhuxClientResult::NoValue {
        return String::new();
    }
    assert_eq!(result, PhuxClientResult::Ok);
    // SAFETY: the spans live until the next mutable call; copied here.
    let cells = unsafe { std::slice::from_raw_parts(grid.cells, grid.cell_count) };
    let arena = if grid.utf8.len == 0 {
        &[]
    } else {
        // SAFETY: as above.
        unsafe { std::slice::from_raw_parts(grid.utf8.data, grid.utf8.len) }
    };
    let mut text = String::new();
    for row in cells.chunks(usize::from(grid.cols)) {
        for cell in row {
            let start = cell.utf8_offset as usize;
            text.push_str(&String::from_utf8_lossy(
                &arena[start..start + usize::from(cell.utf8_len)],
            ));
        }
        text.push('\n');
    }
    text
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one connect, attach, flood and report scenario"
)]
#[ignore = "a measurement; run explicitly with --run-ignored"]
fn output_flood_publications_per_frame() {
    run_local(async {
        let lines = env_or("PHUX_FLOOD_LINES", 65_536);
        let tick = Duration::from_millis(env_or("PHUX_FLOOD_TICK_MS", 16));
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let socket_text = socket.to_str().unwrap().to_owned();
        let (shutdown, server) = spawn_server_with_seed_cmd(
            socket.clone(),
            "flood",
            portable_pty::CommandBuilder::new("/bin/sh"),
        );
        let options = PhuxConnectOptions {
            size: std::mem::size_of::<PhuxConnectOptions>(),
            version: ABI_VERSION,
            base: PhuxClientOptions {
                size: std::mem::size_of::<PhuxClientOptions>(),
                version: ABI_VERSION,
                max_bootstrap_chunk_bytes: 64 * 1024,
                max_history_page_bytes: 64 * 1024,
                max_history_page_rows: 500,
                max_history_cache_bytes: 4 * 1024 * 1024,
                max_history_materialized_rows: 1000,
                history_prefetch_rows: 100,
            },
            target: PhuxBytes::default(),
            socket_path: span(&socket_text),
            config_path: PhuxBytes::default(),
            client_name: span("ffi-flood"),
            wake: None,
            wake_context: std::ptr::null_mut(),
        };
        let mut client: *mut PhuxClient = std::ptr::null_mut();
        // SAFETY: readable options and spans, writable client output.
        assert_eq!(
            unsafe { phux_client_connect(&raw const options, &raw mut client) },
            PhuxClientResult::Ok
        );
        let mut poller = Poller {
            client,
            polls: 0,
            slowest: Duration::ZERO,
        };
        let fast = Duration::from_millis(5);
        poller
            .until("negotiation", fast, |client| {
                // SAFETY: a live client.
                unsafe { phux_client_state(client) == PhuxClientState::Negotiated }
            })
            .await;
        let attach = PhuxAttachOptions {
            size: std::mem::size_of::<PhuxAttachOptions>(),
            version: ABI_VERSION,
            attach_id: 1,
            target_kind: 1,
            session_id: 0,
            name: span("flood"),
            cols: 120,
            rows: 40,
            has_pixel_size: false,
            pixel_width: 0,
            pixel_height: 0,
            request_scrollback: false,
            scrollback_limit_lines: 0,
        };
        // SAFETY: a live client and readable options.
        assert_eq!(
            unsafe { phux_client_queue_attach(client, &raw const attach) },
            PhuxClientResult::Ok
        );
        poller
            .until("the workspace catalog", fast, |client| {
                let mut info = PhuxWorkspaceInfo::default();
                // SAFETY: a live client and a writable output.
                unsafe { phux_client_workspace_info(client, &raw mut info) };
                info.terminal_count > 0
            })
            .await;
        let mut catalog = PhuxCatalogTerminal::default();
        // SAFETY: a live client and a writable output.
        assert_eq!(
            unsafe { phux_client_catalog_terminal_get(client, 0, &raw mut catalog) },
            PhuxClientResult::Ok
        );
        let terminal = catalog.terminal_id;
        poller
            .until("a shell prompt", fast, |client| {
                !grid_text(client, &terminal).trim().is_empty()
            })
            .await;

        // Deterministic: `lines` numbered 72-byte lines, then a marker the
        // command's own echo cannot contain.
        let command = format!(
            "awk 'BEGIN{{for(i=0;i<{lines};i++)printf \"%07d abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ\\n\",i}}'; printf '%s%s\\n' FLOOD_ DONE\r"
        );
        let polls = poller.polls;
        poller.slowest = Duration::ZERO;
        let before = counters(client);
        let (allocs, alloc_bytes) = (
            ALLOCS.load(Ordering::Relaxed),
            ALLOC_BYTES.load(Ordering::Relaxed),
        );
        let started = Instant::now();
        // SAFETY: a live client, readable ID and text.
        assert_eq!(
            unsafe {
                phux_client_send_paste(
                    client,
                    &raw const terminal,
                    command.as_ptr(),
                    command.len(),
                    true,
                )
            },
            PhuxClientResult::Ok
        );
        poller
            .until("the flood's marker", tick, |client| {
                grid_text(client, &terminal).contains("FLOOD_DONE")
            })
            .await;
        let elapsed = started.elapsed();
        let after = counters(client);
        let [frames, batches, published] = [0, 1, 2].map(|index| after[index] - before[index]);
        let allocs = ALLOCS.load(Ordering::Relaxed) - allocs;
        let alloc_bytes = ALLOC_BYTES.load(Ordering::Relaxed) - alloc_bytes;
        let polls = poller.polls - polls;
        println!(
            "flood lines={lines} vt_bytes={} tick_ms={} elapsed_ms={} polls={polls} slowest_poll_ms={:.2}",
            lines * 72,
            tick.as_millis(),
            elapsed.as_millis(),
            poller.slowest.as_secs_f64() * 1e3,
        );
        println!(
            "kernel.frames={frames} runtime.apply_batches={batches} runtime.publish={published} \
             frames_per_publish={:.1}",
            frames as f64 / published.max(1) as f64,
        );
        println!(
            "poll_thread allocs={allocs} ({:.1}/frame) alloc_bytes={alloc_bytes} ({:.0}/frame)",
            allocs as f64 / frames.max(1) as f64,
            alloc_bytes as f64 / frames.max(1) as f64,
        );
        assert!(frames > 0, "the flood reached the kernel");

        // SAFETY: a live client, uniquely owned, on its owning thread.
        unsafe { phux_client_free(client) };
        drop(shutdown);
        server.await.unwrap().unwrap();
    });
}
