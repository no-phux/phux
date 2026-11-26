//! The headless budget: a benchmark's phases and bytes, through a real
//! runtime client and a real Rune view build, with no window and no server.
//!
//! Each tick feeds output, marks the view stale and runs one Rune frame. The
//! counters are exact work budgets, so a scroll that rebuilds every row fails
//! `cargo test`. Times are evidence, not budgets: `cargo test --release
//! perf_budget -- --nocapture` prints one JSON line per phase.
//!
//! The counters are process-wide and other tests bump them, so `perf_budget`
//! runs itself alone in a child process of this test binary.
//!
//! `window_stream` draws the real window (sidebar, tabs, status line, one
//! pane), so chrome that starts rebuilding a pane shows up there too.
//! The same embedded fixture checks key delivery through that window.
//! `crowd` streams through that window's poll too, beside a thousand idle
//! terminals in other sessions.

use std::sync::Arc;
use std::time::Instant;

use phux_client_runtime::{Client, ViewId};
use phux_perf::{MetricValue, PerfReport};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::FrameKind;
use rune::{Cx, El, Env, Runtime, row};

use super::{Bold, DEFAULT_FONT_SIZE, Metrics, paint};
use crate::desk::Shown;
use crate::perf;

mod crowd;

use crate::test_support::input_fixture;
use crate::test_support::terminal::{FILL, Replica};
const SCALE: f32 = 2.0;
const SIZE: (f32, f32) = (1100.0, 720.0);

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
        let selection = crate::theme::Selection::system();
        out = out.child(paint(
            &frame,
            true,
            true,
            &state.metrics,
            selection,
            Bold::default(),
        ));
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
    let mut runtime = crate::test_support::runtime(state, panes, SIZE, Env::new(true, SCALE));
    runtime.state.metrics =
        rune::text::lend(&mut runtime.text, || Metrics::new(DEFAULT_FONT_SIZE, SCALE));
    runtime.frame(0.0);
    (replica, runtime)
}

#[test]
fn fixture_metrics_match_the_runtime_font_advance() {
    let _fonts = rune::text::with_families(super::FAMILIES.0, "Inter");
    let (_, mut runtime) = fixture(80, 24, 1);
    let style = runtime.state.metrics.style(false, false).tracking(0.0);
    let natural = runtime
        .text
        .layout_rich("MMMMMMMMMM", &[], style, 1.0, None)
        .full_width()
        / 10.0;
    let expected = (natural * SCALE).round().max(1.0) / SCALE;
    assert!(natural > 0.0, "the runtime font shapes real advances");
    assert_eq!(
        runtime.state.metrics.cell_w, expected,
        "cached metrics use the runtime's fonts, not the preceding family override"
    );
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
    let mut desk = crate::test_support::window();
    desk.connection.client = Some(Arc::clone(&replica.client));
    desk.connection
        .seed_view_sources(vec![1; 16], &replica.sources);
    desk.connection.layout = Some(phux_client_core::layout::LayoutNode::Leaf(
        replica.terminal.clone(),
    ));
    desk.connection.shown = Some(Shown {
        resource: replica.terminal.clone(),
        view: Some(replica.views[0]),
    });
    let mut runtime =
        crate::test_support::runtime(desk, crate::ui::view, SIZE, Env::new(true, SCALE));
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

/// A Kitty transmit-and-place of a 128x128 image whose colour `index` picks,
/// as image 1 placement 1 over 40x20 cells at the top left: each call
/// replaces the last, as an animating program does.
fn kitty_frame(index: u64) -> String {
    const SIDE: usize = 128;
    let shade = (index % 256) as u8;
    let rgba: Vec<u8> = [shade, 255 - shade, 128, 255].repeat(SIDE * SIDE);
    format!(
        "\x1b[1;1H\x1b_Ga=T,f=32,s={SIDE},v={SIDE},i=1,p=1,c=40,r=20,C=1,q=2;{}\x1b\\",
        base64(&rgba)
    )
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize])
            } else {
                '='
            });
        }
    }
    out
}

/// A still image is uploaded once however often the pane redraws; a looping
/// animation once per new frame. `tick_us` is the pacing record.
fn image_phases() {
    const FRAMES: u64 = 120;
    let (mut replica, mut runtime) = fixture(80, 24, 1);
    replica.feed(kitty_frame(0));
    draw(&mut runtime);
    let uploads = || perf::IMAGE_UPLOADS.get();
    let before = uploads();
    let still = phase("image_still", FRAMES, |index| {
        let letter = if index % 2 == 0 { 'X' } else { 'Y' };
        replica.feed(format!("\x1b[24;1H{letter}"));
        draw(&mut runtime);
    });
    assert_eq!(still.patched, FRAMES, "image_still redraws the pane");
    assert_eq!(uploads() - before, 0, "image_still uploads nothing again");
    let before = uploads();
    phase("image_animation", FRAMES, |index| {
        replica.feed(kitty_frame(index + 1));
        draw(&mut runtime);
    });
    assert_eq!(uploads() - before, FRAMES, "one upload per animation frame");
}

/// Set in the child that does the measuring.
const ALONE: &str = "RUNE_BUDGET_ALONE";

#[test]
fn non_typing_keys_follow_runtime_input_and_stop_at_settings() {
    use phux_protocol::input::key::{KeyAction, ModSet, PhysicalKey};
    use rune::{Key, KeyEvent, Mods};

    let (client, mut runtime) = input_fixture();
    runtime.frame(0.0);
    for (key, physical) in [
        (Key::Insert, PhysicalKey::Insert),
        (Key::PrintScreen, PhysicalKey::PrintScreen),
        (Key::ScrollLock, PhysicalKey::ScrollLock),
        (Key::Pause, PhysicalKey::Pause),
    ] {
        for sheet_open in [false, true] {
            runtime.update(|desk, _| desk.settings_open = sheet_open);
            let _ = client.take_outbound();
            runtime.key(KeyEvent {
                key: key.clone(),
                // Alt, not Shift: Shift+Insert is Paste, what Omarchy's
                // Super+V sends a terminal.
                mods: Mods {
                    alt: true,
                    ..Mods::default()
                },
            });
            runtime.frame(1.0 / 60.0);
            let sent: Vec<_> = client
                .take_outbound()
                .iter()
                .filter_map(
                    |bytes| match FrameKind::decode(bytes).expect("decode input").0 {
                        FrameKind::InputKey { terminal_id, event } => Some((terminal_id, event)),
                        _ => None,
                    },
                )
                .collect();
            if sheet_open {
                assert!(sent.is_empty(), "{key:?} went through Settings");
            } else {
                assert_eq!(sent.len(), 1, "{key:?}");
                let (terminal, event) = &sent[0];
                assert_eq!(terminal, &ResourceId::local(1));
                assert_eq!(event.key, physical);
                assert_eq!(event.action, KeyAction::Press);
                assert_eq!(event.mods, ModSet::ALT);
                assert_eq!(event.text, None);
            }
        }
    }
}

#[test]
fn perf_budget() {
    if std::env::var_os(ALONE).is_some() {
        terminal_phases();
        wide_phases();
        split_phase();
        window_phase();
        image_phases();
        crowd::phases();
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
    assert!(stdout.contains("crowd_1024"), "no budget ran: {stdout}");
    for line in stdout.lines().filter(|line| line.starts_with('{')) {
        println!("{line}");
    }
}
