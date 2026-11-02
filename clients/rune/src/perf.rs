//! Host telemetry: what building terminal panes costs the UI thread.
//!
//! The rows carry the names the GPUI host's `desktop.*` table uses, so the
//! two clients read side by side. A pane renders once per view build
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
//! records every frame's timing there with `rune-perf`.

use std::path::PathBuf;
use std::sync::LazyLock;
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

/// The `rune-perf` recorder `RUNE_PERF_DIR` asks for, and the hook that feeds
/// it each frame.
pub fn frame_recorder() -> Option<impl FnMut(&FrameEncoded) + 'static> {
    let directory = path_from("RUNE_PERF_DIR")?;
    let config = rune_perf::Config::new(directory, "phux-rune").build(crate::updates::VERSION);
    let recorder = match rune_perf::Recorder::start(config) {
        Ok(recorder) => recorder,
        Err(error) => {
            eprintln!("RUNE_PERF_DIR: {error}");
            return None;
        }
    };
    Some(move |frame: &FrameEncoded| {
        let outcome = match frame.outcome() {
            FrameOutcome::Drawn => rune_perf::Outcome::Drawn,
            FrameOutcome::Unchanged => rune_perf::Outcome::Unchanged,
            _ => rune_perf::Outcome::Skipped,
        };
        recorder.record(
            rune_perf::Frame::new(1, frame.seq(), outcome)
                .update_ms(frame.update_ms())
                .render_ms(frame.encode_ms()),
        );
    })
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
