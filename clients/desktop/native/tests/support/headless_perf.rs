//! Main-thread GPU fixture support; shared runtime and pixel helpers live in pixels.rs.

use super::*;

/// A trackpad gesture moves a fraction of a row per event, so most steps stay
/// inside a row and some cross one. Three eighths is exact in binary, so the
/// crossings, and the budgeted work, are the same on every platform.
const WHEEL_ROWS: f32 = 0.375;

/// An opt-in, in-process render benchmark. It reuses the pixel fixture's
/// embedded runtime and main-thread GPUI renderer; screenshots are outside
/// the timed path but prove that draws reach a real offscreen surface.
pub(super) fn headless_perf() {
    const FRAMES: usize = 128;
    const FILL: &str = "terminal workload terminal workload terminal workload terminal workload ";
    let payload = (0..24)
        .map(|row| format!("{row:02} {FILL}"))
        .collect::<Vec<_>>()
        .join("\r\n");
    let (client, view) = publish_at(payload.as_bytes(), 80, 24);
    let mut cx = gpu();
    let in_view = Arc::clone(&client);
    let window = cx
        .open_window(size(px(960.), px(600.)), move |window, cx| {
            cx.new(|cx| Terminal::new(in_view, view, terminal(), settings(false), window, cx))
        })
        .expect("offscreen benchmark window");
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
        .expect("first draw");
    let before = cx
        .capture_screenshot(window.into())
        .expect("initial pixels");

    // Each tick notifies the terminal, as the desk does for a terminal whose
    // runtime published; without it GPUI draws the view from its last frame.
    // A tick with no new output measures the painter's scene reuse.
    let mut draw = || {
        window
            .update(&mut cx, |_, _, cx| cx.notify())
            .expect("notify");
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
            .expect("draw");
    };
    bench_phase("cached", FRAMES, |_| draw());
    let mut seq = 0_u64;
    bench_phase("sparse", FRAMES, |index| {
        seq += 1;
        let letter = if index % 2 == 0 { b'X' } else { b'Y' };
        client
            .feed(FrameKind::ResourceOutput {
                terminal_id: terminal(),
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                seq,
                bytes: [b"\x1b[13;1H".as_slice(), &[letter]].concat().into(),
            })
            .expect("sparse output");
        draw();
    });
    // Every row changes, as in cmatrix. Warm ASCII glyph tables must survive
    // full-screen invalidation: this phase makes zero platform shaping calls.
    bench_phase("ascii_redraw", FRAMES, |index| {
        seq += 1;
        let text = (0..24)
            .map(|row| {
                let line: String = (0..79)
                    .map(|col| char::from(b' ' + ((index + row * 7 + col * 11) % 95) as u8))
                    .collect();
                format!("\x1b[{};1H{line}", row + 1)
            })
            .collect::<String>();
        client
            .feed(FrameKind::ResourceOutput {
                terminal_id: terminal(),
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                seq,
                bytes: text.into_bytes().into(),
            })
            .expect("ASCII animation output");
        draw();
    });
    bench_phase("scroll", FRAMES, |index| {
        seq += 1;
        client
            .feed(FrameKind::ResourceOutput {
                terminal_id: terminal(),
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                seq,
                bytes: format!("\x1b[24;1H{index:04} {FILL}\r\n")
                    .into_bytes()
                    .into(),
            })
            .expect("scroll output");
        draw();
    });
    drop(draw);
    let geometry = window
        .update(&mut cx, |terminal, _, _| terminal.painted())
        .expect("painted")
        .geometry;
    bench_phase("wheel", FRAMES, |_| {
        wheel_step(&mut cx, window, geometry, WHEEL_ROWS)
    });
    let after = cx.capture_screenshot(window.into()).expect("final pixels");
    assert_ne!(
        before.as_raw(),
        after.as_raw(),
        "output must change visible pixels"
    );
    drop(cx);
    drop(client);
    headless_perf_wide();
    window_perf::headless_perf_window(|name, frames, paints, tick| {
        bench_phase_with(name, frames, paints, tick);
    });
}

/// A 200x60 terminal of mixed styling: faint and bold prose, colour runs,
/// spaces between words, and box drawing. Phases redraw it cached, rewrite
/// every row (each stamped with the frame, so no scroll matches), and scroll it.
pub(super) fn headless_perf_wide() {
    const FRAMES: usize = 64;
    const COLS: u16 = 200;
    const ROWS: u16 = 60;
    let screen = |frame: usize| {
        (0..ROWS)
            .map(|row| {
                format!(
                    "\x1b[{};1H{frame:05} {}",
                    row + 1,
                    wide_line(usize::from(row) + frame, COLS - 6)
                )
            })
            .collect::<String>()
    };
    let (client, view) = publish_at(screen(0).as_bytes(), COLS, ROWS);
    let mut cx = gpu();
    let in_view = Arc::clone(&client);
    let window = cx
        .open_window(size(px(2100.), px(1300.)), move |window, cx| {
            cx.new(|cx| Terminal::new(in_view, view, terminal(), settings(false), window, cx))
        })
        .expect("offscreen benchmark window");
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
        .expect("first draw");
    let before = cx
        .capture_screenshot(window.into())
        .expect("initial pixels");
    // Each tick notifies the terminal, as the desk does for a terminal whose
    // runtime published; without it GPUI draws the view from its last frame.
    // A tick with no new output measures the painter's scene reuse.
    let mut draw = || {
        window
            .update(&mut cx, |_, _, cx| cx.notify())
            .expect("notify");
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
            .expect("draw");
    };
    let mut seq = 0_u64;
    let mut feed = |bytes: Vec<u8>| {
        seq += 1;
        client
            .feed(FrameKind::ResourceOutput {
                terminal_id: terminal(),
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                seq,
                bytes: bytes.into(),
            })
            .expect("wide output");
    };
    bench_phase("wide_cached", FRAMES, |_| draw());
    bench_phase("wide_redraw", FRAMES, |index| {
        feed(screen(index + 1).into_bytes());
        draw();
    });
    bench_phase("wide_scroll", FRAMES, |index| {
        feed(format!("\x1b[{ROWS};1H{}\r\n", wide_line(index * 7, COLS)).into_bytes());
        draw();
    });
    drop(draw);
    drop(feed);
    let geometry = window
        .update(&mut cx, |terminal, _, _| terminal.painted())
        .expect("painted")
        .geometry;
    bench_phase("wide_wheel", FRAMES, |_| {
        wheel_step(&mut cx, window, geometry, WHEEL_ROWS)
    });
    let after = cx.capture_screenshot(window.into()).expect("final pixels");
    assert_ne!(
        before.as_raw(),
        after.as_raw(),
        "output must change visible pixels"
    );
    drop(cx);
    drop(client);
}

/// One row of exactly `cols` single-width cells, varied by `seed`.
pub(super) fn wide_line(seed: usize, cols: u16) -> String {
    const SEGMENTS: &[(&str, &str)] = &[
        ("2", "12:04:31.118 "),
        ("0", "\u{2502} "),
        ("1;34", "build"),
        ("0", " compiled the crate in several passes, "),
        ("32", "ok"),
        ("0", " \u{250c}\u{2500}\u{2500}\u{2500}\u{2510} "),
        ("2;3", "faint italic commentary about the step "),
        ("33", "warning: unused variable "),
        ("0", "\u{2514}\u{2500}\u{2500}\u{2500}\u{2518} "),
        ("2", "(dimmed trailer text) "),
        ("7", " selected "),
        ("0", " plain words between \u{2550}\u{2550}\u{2550} rules "),
    ];
    let mut line = String::new();
    let mut width = 0_usize;
    let cols = usize::from(cols);
    for index in 0.. {
        let (sgr, text) = SEGMENTS[(seed + index) % SEGMENTS.len()];
        line.push_str(&format!("\x1b[0;{sgr}m"));
        for ch in text.chars() {
            if width == cols {
                line.push_str("\x1b[0m");
                return line;
            }
            line.push(ch);
            width += 1;
        }
    }
    unreachable!("the loop returns once the row is full")
}

pub(super) fn bench_phase(name: &str, frames: usize, mut tick: impl FnMut(usize)) {
    bench_phase_with(name, frames, true, &mut tick);
}

/// Time `frames` ticks and print one JSON line. `paints` asserts every tick
/// painted the terminal; whole-window phases whose terminal is idle report
/// the paint count instead.
pub(super) fn bench_phase_with(
    name: &str,
    frames: usize,
    paints: bool,
    tick: &mut dyn FnMut(usize),
) {
    let before = phux_perf::snapshot(
        "desktop",
        phux_desktop::perf::TABLE,
        phux_desktop::perf::uptime(),
    );
    let runtime_before = phux_perf::snapshot(
        "runtime",
        phux_client_runtime::perf::TABLE,
        phux_desktop::perf::uptime(),
    );
    let mut elapsed = Vec::with_capacity(frames);
    for index in 0..frames {
        let started = Instant::now();
        tick(index);
        elapsed.push(started.elapsed().as_secs_f64() * 1_000_000.);
    }
    let runtime = phux_perf::snapshot(
        "runtime",
        phux_client_runtime::perf::TABLE,
        phux_desktop::perf::uptime(),
    )
    .delta(&runtime_before);
    elapsed.sort_by(f64::total_cmp);
    let delta = phux_perf::snapshot(
        "desktop",
        phux_desktop::perf::TABLE,
        phux_desktop::perf::uptime(),
    )
    .delta(&before);
    let renders = delta.metric("desktop.render").expect("render counter");
    if paints {
        assert_eq!(
            renders.value,
            phux_perf::MetricValue::Counter(frames as u64),
            "{name}: every measured tick must paint"
        );
    }
    let histogram = |name: &str| match &delta
        .metric(name)
        .or_else(|| runtime.metric(name))
        .expect("stage metric")
        .value
    {
        phux_perf::MetricValue::Histogram(value) => serde_json::json!({
            "count": value.count,
            "mean_us": value.mean(),
            "p50_us": value.percentile(50),
            "p99_us": value.percentile(99),
        }),
        _ => panic!("{name} is not a histogram"),
    };
    let counter = |name: &str| match &delta
        .metric(name)
        .or_else(|| runtime.metric(name))
        .expect("counter metric")
        .value
    {
        phux_perf::MetricValue::Counter(value) => *value,
        _ => panic!("{name} is not a counter"),
    };
    println!(
        "{}",
        serde_json::json!({
            "phase": name,
            "frames": frames,
            "tick_us": {
                "mean": elapsed.iter().sum::<f64>() / frames as f64,
                "p50": elapsed[frames / 2],
                "p99": elapsed[frames * 99 / 100],
            },
            "stages": {
                "acquire": histogram("desktop.acquire"),
                "prepare": histogram("desktop.prepare"),
                "paint": histogram("desktop.paint"),
                "present": histogram("desktop.present"),
            },
            "scene": {
                "terminal_renders": counter("desktop.render"),
                "reused": counter("desktop.prepare_reused"),
                "patched": counter("desktop.prepare_patched"),
                "shaped_lines": counter("desktop.shaped"),
                "platform_shapes": counter("desktop.platform_shaped"),
                "ascii_lines": counter("desktop.ascii_lines"),
            },
            "runtime": {
                "project_grid": histogram("runtime.project_grid"),
                "published": counter("runtime.publish"),
                "buffer_fresh": counter("runtime.buffer_fresh"),
                "owner_turns": histogram("runtime.request_wall")["count"],
            },
            "process": delta.process,
        })
    );
}
