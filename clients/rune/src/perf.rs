//! Host telemetry: what building terminal panes costs the UI thread.
//!
//! A pane renders once per view build
//! (`rune.render`): it acquires its frame (`rune.acquire`), prepares it
//! (`rune.prepare`: reusing the whole pane when its frame didn't move,
//! `rune.prepare_reused`, else keeping unchanged rows and rebuilding the
//! rest, `rune.prepare_patched`), and each rebuilt row counts in
//! `rune.rows_built` and its text elements in `rune.shaped`.
//!
//! Everything is a `static` from [`phux_perf`], always on, one relaxed atomic
//! add per sample. Three environment variables read it out, none on by
//! default: `RUNE_PERF_HUD` draws a line over the terminal area,
//! `RUNE_PERF_JSON=FILE` keeps a report in `FILE`, and `RUNE_PERF_DIR=DIR`
//! records every frame's timing there with `rune-perf`. `PHUX_ECHO_HOPS`
//! adds Rune's hops to the keystroke-echo trace (DEVELOP.md).

use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use phux_perf::{Counter, Histogram, Metric, Unit};
use rune::{El, FrameEncoded, FrameOutcome, text};

/// Terminal panes built: one per pane per view build.
pub static RENDERS: Counter = Counter::new();
/// Microseconds to acquire one pane's frame.
pub static ACQUIRE: Histogram = Histogram::new();
/// Acquires that found no frame: the pane shows its placeholder.
pub static ACQUIRE_REJECTED: Counter = Counter::new();
/// Microseconds to prepare one pane, draw list and row checks included.
pub static PREPARE: Histogram = Histogram::new();
/// Prepares that reused the pane's previous subtree.
pub static PREPARE_REUSED: Counter = Counter::new();
/// Prepares that walked the frame and kept its unchanged rows.
pub static PREPARE_PATCHED: Counter = Counter::new();
/// Rows built again.
pub static ROWS_BUILT: Counter = Counter::new();
/// Text elements built: Rune shapes each one it has not seen.
pub static SHAPED: Counter = Counter::new();

/// The table, in render order.
pub static TABLE: &[Metric] = &[
    Metric::counter("rune.render", Unit::Count, &RENDERS),
    Metric::histogram("rune.acquire", Unit::Micros, &ACQUIRE),
    Metric::counter("rune.acquire_rejected", Unit::Count, &ACQUIRE_REJECTED),
    Metric::histogram("rune.prepare", Unit::Micros, &PREPARE),
    Metric::counter("rune.prepare_reused", Unit::Count, &PREPARE_REUSED),
    Metric::counter("rune.prepare_patched", Unit::Count, &PREPARE_PATCHED),
    Metric::counter("rune.rows_built", Unit::Count, &ROWS_BUILT),
    Metric::counter("rune.shaped", Unit::Count, &SHAPED),
];

static STARTED: LazyLock<Instant> = LazyLock::new(Instant::now);
static HUD: LazyLock<bool> = LazyLock::new(|| std::env::var_os("RUNE_PERF_HUD").is_some());

fn path_from(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Keep `RUNE_PERF_JSON`'s file current. A headless run exits without a
/// hook, so a thread rewrites it rather than one report at the end.
pub fn report_to_file() {
    LazyLock::force(&STARTED);
    let Some(path) = path_from("RUNE_PERF_JSON") else {
        return;
    };
    std::thread::spawn(move || {
        loop {
            let report = phux_perf::snapshot("client", TABLE, STARTED.elapsed());
            if let Err(error) = std::fs::write(&path, report.to_json()) {
                eprintln!("RUNE_PERF_JSON {}: {error}", path.display());
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
}

/// The focused pane's last built publication, and whether a frame has
/// drawn it yet: the keystroke-echo trace's `build` and `stdout` hops.
static BUILT_SEQ: AtomicU64 = AtomicU64::new(u64::MAX);
static UNDRAWN: AtomicBool = AtomicBool::new(false);

/// The focused pane built a frame: `build` when it carries a publication
/// the last build didn't, so a cursor blink is not an echo.
pub fn built(last_seq: u64) {
    if BUILT_SEQ.swap(last_seq, Ordering::Relaxed) != last_seq {
        phux_perf::echo_hop::record("build");
        UNDRAWN.store(true, Ordering::Relaxed);
    }
}

/// The hook for every frame the window ran: `stdout` once a drawn frame
/// shows the last `build` (window: right after present; headless: after
/// submit), and the `rune-perf` recorder `RUNE_PERF_DIR` asks for.
pub fn frame_hook() -> Option<impl FnMut(&FrameEncoded) + 'static> {
    let hops = phux_perf::echo_hop::enabled();
    let recorder = frame_recorder();
    if !hops && recorder.is_none() {
        return None;
    }
    Some(move |frame: &FrameEncoded| {
        let outcome = match frame.outcome() {
            FrameOutcome::Drawn => rune_perf::Outcome::Drawn,
            FrameOutcome::Unchanged => rune_perf::Outcome::Unchanged,
            _ => rune_perf::Outcome::Skipped,
        };
        if hops && outcome == rune_perf::Outcome::Drawn && UNDRAWN.swap(false, Ordering::Relaxed) {
            phux_perf::echo_hop::record("stdout");
        }
        if let Some(recorder) = &recorder {
            recorder.record(
                rune_perf::Frame::new(1, frame.seq(), outcome)
                    .update_ms(frame.update_ms())
                    .render_ms(frame.encode_ms()),
            );
        }
    })
}

fn frame_recorder() -> Option<rune_perf::Recorder> {
    let directory = path_from("RUNE_PERF_DIR")?;
    let config = rune_perf::Config::new(directory, "phux-rune").build(crate::updates::VERSION);
    rune_perf::Recorder::start(config)
        .inspect_err(|error| eprintln!("RUNE_PERF_DIR: {error}"))
        .ok()
}

/// What the last view build came to, for `RUNE_PERF_HUD`. It redraws only
/// with the view, so it never keeps an idle window awake.
pub fn hud(color: rune::Color) -> Option<El> {
    if !*HUD {
        return None;
    }
    let prepare = PREPARE.snapshot().mean();
    let line = format!(
        "panes {}  reused {}  patched {}  rows {}  text {}  prepare {prepare}µs",
        RENDERS.get(),
        PREPARE_REUSED.get(),
        PREPARE_PATCHED.get(),
        ROWS_BUILT.get(),
        SHAPED.get(),
    );
    Some(
        text(line)
            .key("perf-hud")
            .absolute(8.0, 4.0)
            .font(crate::ui::CAPTION.family(rune::Family::SystemMono))
            .color(color)
            .single_line()
            .inert(),
    )
}

#[cfg(test)]
mod tests {
    use super::TABLE;

    #[test]
    fn names_are_unique_and_prefixed() {
        let mut names: Vec<&str> = TABLE.iter().map(|m| m.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
        assert!(names.iter().all(|name| name.starts_with("rune.")));
    }
}
