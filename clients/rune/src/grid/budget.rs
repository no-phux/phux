//! The headless budget: the GPUI benchmark's phases and bytes, through a real
//! runtime client and a real Rune view build, with no window and no server.
//!
//! Each tick feeds output, marks the view stale and runs one Rune frame. The
//! counters are exact work budgets, so a scroll that rebuilds every row fails
//! `cargo test`. Times are evidence, not budgets: `cargo test --release
//! perf_budget -- --nocapture` prints one JSON line per phase, in the shape
//! `clients/desktop/scripts/perf-budget.ts` reads.
//!
//! The counters are process-wide and other tests bump them, so `perf_budget`
//! runs itself alone in a child process of this test binary.
//!
//! `window_stream` draws the real window (sidebar, tabs, status line, one
//! pane), so chrome that starts rebuilding a pane shows up there too.

use std::sync::Arc;
use std::time::Instant;

use phux_client_runtime::control::{ControlOptions, ControlPlane};
use phux_client_runtime::{Client, Runtime as ClientRuntime, ViewId};
use phux_perf::{MetricValue, PerfReport};
use phux_protocol::ids::{BootstrapId, ClientId, ResourceId, SessionId, StreamId, WindowId};
use phux_protocol::wire::frame::{AttachTarget, FrameKind};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
use phux_protocol::{
    PROTOCOL_VERSION,
    caps::{BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities},
};
use rune::{Cx, El, Env, Runtime, row};

use super::{DEFAULT_FONT_SIZE, Metrics, paint};
use crate::desk::{Desk, Shown};
use crate::launch::Resolved;
use crate::perf;

const FILL: &str = "terminal workload terminal workload terminal workload terminal workload ";
const SCALE: f32 = 2.0;
const SIZE: (f32, f32) = (1100.0, 720.0);

fn stream() -> StreamId {
    StreamId::new(1).expect("a stream id")
}

fn bootstrap() -> BootstrapId {
    BootstrapId::new(1).expect("a bootstrap id")
}

/// An attached embedded client showing `payload` in one view per terminal,
/// the first of which `feed` writes to.
struct Replica {
    client: Arc<Client>,
    views: Vec<ViewId>,
    terminal: ResourceId,
    seq: u64,
}

impl Replica {
    fn new(cols: u16, rows: u16, terminals: u32) -> Self {
        let client = ClientRuntime::embedded(ControlOptions {
            attach: Some(AttachTarget::ByName("budget".into())),
            viewport: (cols, rows),
            ..ControlOptions::default()
        });
        client.with_control(ControlPlane::connection_opened);
        let _ = client.take_outbound();
        client
            .feed(FrameKind::HelloOk {
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new(),
                server_id: vec![1; 16],
                selected_profile: BootstrapProfile::SynthesizedVtRaw,
                bootstrap_limits: BootstrapLimits::default(),
            })
            .expect("hello");
        let attach_id = client
            .take_outbound()
            .iter()
            .find_map(|bytes| match FrameKind::decode(bytes).expect("decode").0 {
                FrameKind::Attach { attach_id, .. } => Some(attach_id),
                _ => None,
            })
            .expect("attach request");
        let ids: Vec<ResourceId> = (1..=terminals).map(ResourceId::local).collect();
        let (session, window) = (SessionId::new(1), WindowId::new(1));
        let resources = ids
            .iter()
            .map(|id| ResourceInfo::new(id.clone(), window, cols, rows))
            .collect();
        let snapshot = SessionSnapshot::new(session, window, ids[0].clone())
            .with_sessions(vec![SessionInfo::new(session, "budget")])
            .with_windows(vec![
                WindowInfo::new(window, session, "shell")
                    .with_active_resource(Some(ids[0].clone())),
            ])
            .with_resources(resources);
        client
            .feed(FrameKind::Attached {
                attach_id,
                snapshot,
                initial_client_id: ClientId::new(1),
            })
            .expect("attached");
        let payload = (0..rows)
            .map(|row| format!("{row:02} {FILL}"))
            .collect::<Vec<_>>()
            .join("\r\n");
        for (index, id) in ids.iter().enumerate() {
            let number = index as u64 + 1;
            let stream_id = StreamId::new(number).expect("a stream id");
            let bootstrap_id = BootstrapId::new(number).expect("a bootstrap id");
            for frame in [
                FrameKind::BootstrapBegin {
                    terminal_id: id.clone(),
                    stream_id,
                    bootstrap_id,
                    profile: BootstrapStreamProfile::SynthesizedVtRaw,
                    cols,
                    rows,
                    base_seq: 0,
                },
                FrameKind::BootstrapChunk {
                    terminal_id: id.clone(),
                    stream_id,
                    bootstrap_id,
                    chunk_seq: 0,
                    payload: payload.clone().into_bytes().into(),
                },
                FrameKind::BootstrapReady {
                    terminal_id: id.clone(),
                    stream_id,
                    bootstrap_id,
                    history_cursor: None,
                },
            ] {
                client.feed(frame).expect("bootstrap");
            }
        }
        client
            .feed(FrameKind::AttachReady { attach_id })
            .expect("attach ready");
        let _ = client.take_outbound();
        let _ = client.take_events();
        let views = ids
            .iter()
            .map(|id| client.create_view(id).expect("a view"))
            .collect();
        Self {
            client: Arc::new(client),
            views,
            terminal: ids[0].clone(),
            seq: 0,
        }
    }

    fn feed(&mut self, bytes: String) {
        self.seq += 1;
        self.client
            .feed(FrameKind::ResourceOutput {
                terminal_id: self.terminal.clone(),
                stream_id: stream(),
                bootstrap_id: bootstrap(),
                seq: self.seq,
                bytes: bytes.into_bytes().into(),
            })
            .expect("output");
    }
}

/// The bench's whole view: panes side by side, nothing else.
struct Panes {
    client: Arc<Client>,
    views: Vec<ViewId>,
    metrics: Metrics,
    tick: u64,
}

fn panes(state: &Panes, _: &mut Cx<Panes>) -> El {
    let mut out = row();
    for view in &state.views {
        perf::RENDERS.incr();
        let frame = state.client.acquire_view(*view).expect("a frame");
        out = out.child(paint(&frame, true, true, &state.metrics));
    }
    out
}

fn counter(report: &PerfReport, name: &str) -> u64 {
    match report.metric(name).map(|metric| &metric.value) {
        Some(MetricValue::Counter(value)) => *value,
        _ => panic!("{name} is not a counter"),
    }
}

/// One phase's work.
#[derive(Debug, PartialEq, Eq)]
struct Work {
    reused: u64,
    patched: u64,
    rows: u64,
    shaped: u64,
}

/// Run `tick` `frames` times; print the phase's line and return its work.
fn phase(name: &str, frames: u64, mut tick: impl FnMut(u64)) -> Work {
    let snapshot = || phux_perf::snapshot("client", perf::TABLE, std::time::Duration::ZERO);
    let before = snapshot();
    let mut micros = Vec::with_capacity(frames as usize);
    for index in 0..frames {
        let started = Instant::now();
        tick(index);
        micros.push(started.elapsed().as_secs_f64() * 1_000_000.0);
    }
    let delta = snapshot().delta(&before);
    micros.sort_by(f64::total_cmp);
    let work = Work {
        reused: counter(&delta, "rune.prepare_reused"),
        patched: counter(&delta, "rune.prepare_patched"),
        rows: counter(&delta, "rune.rows_built"),
        shaped: counter(&delta, "rune.shaped"),
    };
    let mean = micros.iter().sum::<f64>() / micros.len() as f64;
    let prepare = match delta.metric("rune.prepare").map(|metric| &metric.value) {
        Some(MetricValue::Histogram(value)) => value.mean(),
        _ => panic!("rune.prepare is not a histogram"),
    };
    let line = serde_json::json!({
        "phase": name,
        "frames": frames,
        "scene": {
            "reused": work.reused,
            "patched": work.patched,
            "rows": work.rows,
            "shaped_lines": work.shaped,
        },
        "prepare_us": prepare,
        "tick_us": {
            "mean": mean.round(),
            "p50": micros[micros.len() / 2].round(),
            "p99": micros[micros.len() * 99 / 100].round(),
        },
    });
    println!("{line}");
    work
}

fn work(reused: u64, patched: u64, rows: u64, shaped: u64) -> Work {
    Work {
        reused,
        patched,
        rows,
        shaped,
    }
}

fn scroll_line(row: u16, index: u64) -> String {
    format!("\x1b[{row};1H{index:04} {FILL}\r\n")
}

fn fixture(cols: u16, rows: u16, terminals: u32) -> (Replica, Runtime<Panes>) {
    let replica = Replica::new(cols, rows, terminals);
    let state = Panes {
        client: Arc::clone(&replica.client),
        views: replica.views.clone(),
        metrics: Metrics::new(DEFAULT_FONT_SIZE, SCALE),
        tick: 0,
    };
    let mut runtime = Runtime::new(state, panes, SIZE, Env::new(true, SCALE));
    runtime.frame(0.0);
    (replica, runtime)
}

fn draw(runtime: &mut Runtime<Panes>) {
    runtime.update(|state, _| state.tick += 1);
    runtime.frame(1.0 / 60.0);
}

/// Every row changes, as in cmatrix: printable ASCII stamped by the frame.
fn redraw(rows: u16, cols: u16, index: u64) -> String {
    (0..u64::from(rows))
        .map(|row| {
            let line: String = (0..u64::from(cols) - 1)
                .map(|col| char::from(b' ' + ((index + row * 7 + col * 11) % 95) as u8))
                .collect();
            format!("\x1b[{};1H{line}", row + 1)
        })
        .collect()
}

fn terminal_phases() {
    const FRAMES: u64 = 128;
    let (mut replica, mut runtime) = fixture(80, 24, 1);
    let cached = phase("cached", FRAMES, |_| draw(&mut runtime));
    assert_eq!(cached, work(FRAMES, 0, 0, 0), "cached");
    // The cursor moves to the sparse row first: it is one row after that.
    replica.feed("\x1b[13;1HY".to_owned());
    draw(&mut runtime);
    let sparse = phase("sparse", FRAMES, |index| {
        let letter = if index % 2 == 0 { 'X' } else { 'Y' };
        replica.feed(format!("\x1b[13;1H{letter}"));
        draw(&mut runtime);
    });
    assert_eq!((sparse.patched, sparse.rows), (FRAMES, FRAMES), "sparse");
    let ascii = phase("ascii_redraw", FRAMES, |index| {
        replica.feed(redraw(24, 80, index));
        draw(&mut runtime);
    });
    assert_eq!((ascii.patched, ascii.rows), (FRAMES, 24 * FRAMES), "ascii");
    // One line settles the cursor on the bottom row, as in every scroll below.
    replica.feed(scroll_line(24, 9999));
    draw(&mut runtime);
    let scroll = phase("scroll", FRAMES, |index| {
        replica.feed(scroll_line(24, index));
        draw(&mut runtime);
    });
    assert_eq!((scroll.patched, scroll.rows), (FRAMES, FRAMES), "scroll");
    assert!(!runtime.animating, "renamed rows stay still");
}

fn wide_phases() {
    const FRAMES: u64 = 64;
    let (mut replica, mut runtime) = fixture(200, 60, 1);
    let cached = phase("wide_cached", FRAMES, |_| draw(&mut runtime));
    assert_eq!(cached, work(FRAMES, 0, 0, 0), "wide_cached");
    let redrawn = phase("wide_redraw", FRAMES, |index| {
        replica.feed(redraw(60, 200, index));
        draw(&mut runtime);
    });
    // The pattern repeats a frame back 27 rows away, and those rows are kept.
    assert_eq!(redrawn.rows, 1824, "wide_redraw");
    replica.feed(scroll_line(60, 9999));
    draw(&mut runtime);
    let scroll = phase("wide_scroll", FRAMES, |index| {
        replica.feed(scroll_line(60, index));
        draw(&mut runtime);
    });
    assert_eq!(scroll.rows, FRAMES, "wide_scroll");
}

/// One pane streams beside an idle one: the idle pane is never prepared.
fn split_phase() {
    const FRAMES: u64 = 128;
    let (mut replica, mut runtime) = fixture(80, 40, 2);
    replica.feed(scroll_line(40, 9999));
    draw(&mut runtime);
    let split = phase("split_stream", FRAMES, |index| {
        replica.feed(scroll_line(40, index));
        draw(&mut runtime);
    });
    assert_eq!(split, work(FRAMES, FRAMES, FRAMES, FRAMES), "split_stream");
}

/// The real window around one streaming terminal.
fn window_phase() {
    const FRAMES: u64 = 128;
    let (cols, rows) = (120, 40);
    let mut replica = Replica::new(cols, rows, 1);
    let mut desk = Desk::new(&Resolved::offline("budget"));
    desk.client = Some(Arc::clone(&replica.client));
    desk.layout = Some(phux_client_core::layout::LayoutNode::Leaf(
        replica.terminal.clone(),
    ));
    desk.shown = Some(Shown {
        terminal: replica.terminal.clone(),
        view: replica.views[0],
    });
    let mut runtime = Runtime::new(desk, crate::ui::view, SIZE, Env::new(true, SCALE));
    runtime.frame(0.0);
    replica.feed(scroll_line(rows, 9999));
    runtime.update(|desk, _| desk.generation += 1);
    runtime.frame(1.0 / 60.0);
    let stream = phase("window_stream", FRAMES, |index| {
        replica.feed(scroll_line(rows, index));
        runtime.update(|desk, _| desk.generation += 1);
        runtime.frame(1.0 / 60.0);
    });
    assert_eq!((stream.patched, stream.rows), (FRAMES, FRAMES), "window");
    assert!(!runtime.animating, "output doesn't keep the window drawing");
}

/// Set in the child that does the measuring.
const ALONE: &str = "RUNE_BUDGET_ALONE";

#[test]
fn perf_budget() {
    if std::env::var_os(ALONE).is_some() {
        terminal_phases();
        wide_phases();
        split_phase();
        window_phase();
        return;
    }
    let module = module_path!().split_once("::").map_or("", |(_, rest)| rest);
    let binary = std::env::current_exe().expect("the test binary");
    let output = std::process::Command::new(binary)
        .args(["--exact", &format!("{module}::perf_budget"), "--nocapture"])
        .env(ALONE, "1")
        .output()
        .expect("the budget runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // A renamed module would match no test and pass with nothing measured.
    assert!(stdout.contains("window_stream"), "no budget ran: {stdout}");
    for line in stdout.lines().filter(|line| line.starts_with('{')) {
        println!("{line}");
    }
}
