//! Publication pacing against a real server: how many grids the owner thread
//! projects during an output flood against how many a display-rate consumer
//! reads, and how long a keystroke's echo takes to reach a publication while
//! the consumer is caught up.
//!
//! A measurement, not a gate: its figures depend on scheduling, so it is
//! ignored by default. Run it with
//!
//! ```text
//! cargo nextest run -p phux-client-runtime --test publish_pacing --run-ignored all --no-capture
//! ```
//!
//! `PHUX_FLOOD_LINES` (default 1000000, ~72 MB of VT) sizes the flood,
//! `PHUX_PACING_TICK_MS` (default 22, a ~45 Hz window) the consumer's read
//! interval, and `PHUX_ECHO_SAMPLES` (default 50) the echo probe.

#![cfg(feature = "engine")]
#![allow(clippy::expect_used, reason = "test assertions")]
#![allow(clippy::unwrap_used, reason = "test assertions")]
#![allow(clippy::panic, reason = "test assertions")]
#![allow(
    clippy::print_stdout,
    reason = "the figures are this measurement's output"
)]
#![allow(clippy::cast_precision_loss, reason = "ratios for a printed report")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use phux_client_runtime::control::{ControlOptions, Event, SpawnRequest, Status};
use phux_client_runtime::reconnect::Ladder;
use phux_client_runtime::{Client, ClientOptions, ConnectOptions, Runtime, Target};
use phux_perf::ProcessStats;
use phux_protocol::ResourceId;
use phux_protocol::wire::frame::AttachTarget;
use phux_server_testkit::{run_local, spawn_server};
use tempfile::TempDir;

const DEADLINE: Duration = Duration::from_secs(120);

/// Counts every allocation in the process: server, driver, owner thread and
/// consumer alike. The deltas compare like with like across a change.
struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);

// SAFETY: defers to the system allocator; counting has no effect on layout.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
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

fn options() -> ClientOptions {
    ClientOptions {
        control: ControlOptions {
            client_name: "phux-publish-pacing".to_owned(),
            attach: Some(AttachTarget::ByName("main".to_owned())),
            viewport: (120, 40),
            ..ControlOptions::default()
        },
        connect: ConnectOptions {
            ladder: Ladder::AGENT_VERB,
            ..ConnectOptions::default()
        },
    }
}

async fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(start.elapsed() < DEADLINE, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn spawn(client: &Client, command: &str) -> ResourceId {
    let request_id = client.spawn_terminal(SpawnRequest {
        command: Some(vec![command.to_owned()]),
        ..SpawnRequest::default()
    });
    let start = Instant::now();
    let terminal = loop {
        let spawned = client
            .take_events()
            .into_iter()
            .find_map(|event| match event {
                Event::TerminalSpawned {
                    request_id: id,
                    terminal_id: Some(terminal_id),
                    ..
                } if id == request_id => Some(terminal_id),
                _ => None,
            });
        if let Some(terminal) = spawned {
            break terminal;
        }
        assert!(start.elapsed() < DEADLINE, "timed out spawning {command}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    wait_until("input readiness", || client.input_ready(&terminal)).await;
    terminal
}

/// A `runtime.*` counter by name, zero when this revision has no such
/// metric, so the report runs unchanged on either side of a change.
fn counter(name: &str) -> u64 {
    phux_client_runtime::perf::snapshot()
        .find(|metric| metric.name == name)
        .and_then(|metric| match metric.value {
            phux_perf::MetricValue::Counter(value) => Some(value),
            _ => None,
        })
        .unwrap_or(0)
}

struct Sample {
    acquired: u64,
    deferred: u64,
    caught_up: u64,
    published: u64,
    batches: u64,
    frames: u64,
    project_us: u64,
    cpu_us: u64,
    allocs: u64,
}

impl Sample {
    fn take() -> Self {
        let cpu = ProcessStats::capture().expect("getrusage");
        Self {
            acquired: counter("runtime.acquire"),
            deferred: counter("runtime.publish_deferred"),
            caught_up: counter("runtime.catch_up"),
            published: phux_client_runtime::perf::PUBLISHED.get(),
            batches: phux_client_runtime::perf::APPLY_BATCHES.get(),
            frames: phux_client_core::perf::OUTPUT_FRAMES.get(),
            project_us: phux_client_runtime::perf::PROJECT.snapshot().sum,
            cpu_us: cpu.cpu_user_us + cpu.cpu_system_us,
            allocs: ALLOCS.load(Ordering::Relaxed),
        }
    }
}

fn percentile(sorted: &[Duration], p: usize) -> f64 {
    let index = (sorted.len() * p / 100).min(sorted.len() - 1);
    sorted[index].as_secs_f64() * 1e6
}

#[test]
#[ignore = "a measurement; run explicitly with --run-ignored"]
fn flood_publications_against_display_rate_reads() {
    run_local(async {
        let lines = env_or("PHUX_FLOOD_LINES", 1_000_000);
        let tick = Duration::from_millis(env_or("PHUX_PACING_TICK_MS", 22));
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server(socket.clone(), Some("main"));
        let client = Runtime::connect(Target::uds(&socket), options()).expect("connect");
        wait_until("attach", || client.status() == Status::Attached).await;
        let terminal = spawn(&client, "/bin/sh").await;
        // The desktop's shape: an independent view is what the window reads;
        // the default presentation exists and nobody reads it.
        let view = client.create_view(&terminal).expect("view");
        wait_until("a prompt", || {
            client
                .acquire_view(view)
                .is_some_and(|frame| !frame.text().trim().is_empty())
        })
        .await;

        // Deterministic: `lines` numbered 72-byte lines, then a marker the
        // command's own echo cannot contain. A script keeps the typed line
        // short enough for the tty's canonical-mode buffer.
        let script = tmp.path().join("flood.sh");
        std::fs::write(
            &script,
            format!(
                "awk 'BEGIN{{for(i=0;i<{lines};i++)printf \"%07d abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ\\n\",i}}'\nprintf '%s%s\\n' FLOOD_ DONE\n"
            ),
        )
        .unwrap();
        let command = format!("sh {}\r", script.display());
        let before = Sample::take();
        let started = Instant::now();
        assert!(client.send_text(&terminal, &command));
        let mut reads = 0_u64;
        loop {
            tokio::time::sleep(tick).await;
            reads += 1;
            let frame = client.acquire_view(view).expect("view frame");
            assert!(
                started.elapsed() < DEADLINE,
                "timed out on the flood: {:?}",
                frame.text()
            );
            if frame.text().contains("FLOOD_DONE") {
                break;
            }
        }
        let elapsed = started.elapsed().as_secs_f64();
        let after = Sample::take();
        let published = after.published - before.published;
        println!(
            "flood lines={lines} tick_ms={} elapsed_ms={:.0} reads={reads} ({:.0}/s)",
            tick.as_millis(),
            elapsed * 1e3,
            reads as f64 / elapsed,
        );
        println!(
            "kernel.frames={} runtime.apply_batches={} runtime.publish={published} ({:.0}/s) \
             publish_per_read={:.2}",
            after.frames - before.frames,
            after.batches - before.batches,
            published as f64 / elapsed,
            published as f64 / reads.max(1) as f64,
        );
        println!(
            "runtime.acquire={} runtime.publish_deferred={} runtime.catch_up={}",
            after.acquired - before.acquired,
            after.deferred - before.deferred,
            after.caught_up - before.caught_up,
        );
        println!(
            "runtime.project_total_ms={:.1} process_cpu_ms={:.0} allocs={}",
            (after.project_us - before.project_us) as f64 / 1e3,
            (after.cpu_us - before.cpu_us) as f64 / 1e3,
            after.allocs - before.allocs,
        );

        client.close();
        drop(shutdown);
        server.await.unwrap().unwrap();
    });
}

#[test]
#[ignore = "a measurement; run explicitly with --run-ignored"]
fn keystroke_echo_to_publication_latency() {
    run_local(async {
        let samples = env_or("PHUX_ECHO_SAMPLES", 50);
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server(socket.clone(), Some("main"));
        let client = Runtime::connect(Target::uds(&socket), options()).expect("connect");
        wait_until("attach", || client.status() == Status::Attached).await;
        let terminal = spawn(&client, "/bin/cat").await;
        let view = client.create_view(&terminal).expect("view");
        let slot = client.view_slot(view).expect("view slot");
        let mut latencies = Vec::new();
        for index in 0..samples {
            // A caught-up consumer: it has read the current frame and the
            // terminal is quiet.
            tokio::time::sleep(Duration::from_millis(20)).await;
            let generation = client.acquire_view(view).expect("frame").generation;
            let key = char::from(b'a' + u8::try_from(index % 26).unwrap());
            let sent = Instant::now();
            assert!(client.send_text(&terminal, &key.to_string()));
            while slot.generation() == generation {
                assert!(sent.elapsed() < DEADLINE, "the echo never published");
                tokio::task::yield_now().await;
            }
            latencies.push(sent.elapsed());
        }
        latencies.sort_unstable();
        println!(
            "echo samples={samples} key_to_publish_us p50={:.0} p95={:.0} max={:.0}",
            percentile(&latencies, 50),
            percentile(&latencies, 95),
            percentile(&latencies, 100),
        );

        client.close();
        drop(shutdown);
        server.await.unwrap().unwrap();
    });
}
