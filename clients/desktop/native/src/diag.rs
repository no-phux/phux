//! Diagnostics: one place every layer reports into.
//!
//! - **Log.** `log` records go to stderr and the platform's state/log directory
//!   (`PHUX_DESKTOP_LOG` overrides the path; `RUST_LOG`-style level in
//!   `PHUX_DESKTOP_LOG_LEVEL`, default `info`). Every line is [`redact`]ed
//!   first, and the file rotates to one `.1` backup at [`LOG_CAP`] bytes.
//! - **Receipt.** `Copy Diagnostics` copies a [`Receipt`]: app and runtime
//!   versions, server id, connection epoch, host and the last redacted
//!   warnings. It carries no terminal content.
//! - **Metrics.** [`crate::perf`] owns the always-on counters and histograms;
//!   this module adds the window-level rows (`desktop.frame`, `desktop.drain*`)
//!   and samples everything into `perf.jsonl` once a second when `--evidence`
//!   is set.
//! - **Evidence.** With `--evidence DIR`, the first frame whose exact
//!   drawable the platform reports for a visible window, with at least one
//!   painted terminal, is written as `presented.png` (an offscreen render of
//!   that scene), `layout.json` and `interaction.json`. `presented` in those
//!   files says whether the display confirmed the drawable; a virtual display
//!   (hosted CI) never does. `--smoke` then exits 0. Nothing here writes a
//!   success flag: the files are the proof, and a qualification harness
//!   checks them.
//!
//! Windows never activate under `--evidence`, so qualification runs do not
//! steal focus from the person at the machine.

mod log_file;
mod receipt;
mod redact;

pub use log_file::{LOG_CAP, LogFile};
pub use receipt::{Receipt, development_version};
pub use redact::{REDACTED, redact};

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{App, Global, Window};
use phux_perf::{Counter, Histogram, Metric, Unit};
use serde_json::json;

/// Microseconds the window spent building one frame of the desk.
pub static FRAME: Histogram = Histogram::new();
/// Runtime wakes the desks drained.
pub static DRAIN_WAKES: Counter = Counter::new();
/// Events those wakes carried.
pub static DRAIN_EVENTS: Counter = Counter::new();
/// Microseconds spent draining and applying one wake.
pub static DRAIN: Histogram = Histogram::new();
/// Drawable-presented receipts observed for evidence.
pub static PRESENTED: Counter = Counter::new();

/// Window-level rows, appended to [`crate::perf::TABLE`] in reports.
pub static TABLE: &[Metric] = &[
    Metric::histogram("desktop.frame", Unit::Micros, &FRAME),
    Metric::counter("desktop.drain_wakes", Unit::Count, &DRAIN_WAKES),
    Metric::counter("desktop.drain_events", Unit::Count, &DRAIN_EVENTS),
    Metric::histogram("desktop.drain", Unit::Micros, &DRAIN),
    Metric::counter("desktop.presented", Unit::Count, &PRESENTED),
];

// ── Log ────────────────────────────────────────────────────────────────

struct Logger {
    file: Mutex<Option<LogFile>>,
    level: log::LevelFilter,
}

/// Warnings and errors since launch, for [`recent_errors`].
static RECENT: log_file::Recent = log_file::Recent::new();

/// The last warnings and errors this process logged, redacted, oldest first.
#[must_use]
pub fn recent_errors() -> Vec<String> {
    RECENT.snapshot()
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = redact(&format!(
            "{} {} {} {}\n",
            unix_millis(),
            record.level(),
            record.target(),
            record.args()
        ));
        if record.level() <= log::Level::Warn {
            RECENT.note(&line);
        }
        let _ = std::io::stderr().write_all(line.as_bytes());
        if let Some(file) = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
        {
            file.write_line(line.as_bytes());
        }
    }

    fn flush(&self) {}
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn log_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PHUX_DESKTOP_LOG") {
        return Some(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME")?;
    Some(default_log_path(
        Path::new(&home),
        std::env::var_os("XDG_STATE_HOME"),
    ))
}

fn default_log_path(home: &Path, state: Option<std::ffi::OsString>) -> PathBuf {
    if cfg!(target_os = "linux") {
        state
            .map_or_else(|| home.join(".local/state"), PathBuf::from)
            .join("phux-desktop/desktop.log")
    } else {
        home.join("Library/Logs/phux-desktop.log")
    }
}

#[cfg(test)]
mod path_tests {
    use super::default_log_path;
    use std::path::Path;

    #[test]
    fn logs_use_the_platform_directory() {
        let home = Path::new("/home/person");
        if cfg!(target_os = "linux") {
            assert_eq!(
                default_log_path(home, None),
                home.join(".local/state/phux-desktop/desktop.log")
            );
            assert_eq!(
                default_log_path(home, Some("/state".into())),
                Path::new("/state/phux-desktop/desktop.log")
            );
        } else {
            assert_eq!(
                default_log_path(home, None),
                home.join("Library/Logs/phux-desktop.log")
            );
        }
    }
}

/// Install the process logger once. A log file that cannot be opened leaves
/// stderr logging in place; it never stops the app.
pub fn init_logging() {
    let level = std::env::var("PHUX_DESKTOP_LOG_LEVEL")
        .ok()
        .and_then(|level| level.parse().ok())
        .unwrap_or(log::LevelFilter::Info);
    let file = log_path().and_then(|path| LogFile::open(path, LOG_CAP).ok());
    static LOGGER: OnceLock<Logger> = OnceLock::new();
    let logger = LOGGER.get_or_init(|| Logger {
        file: Mutex::new(file),
        level,
    });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(level);
    }
}

// ── Reports ────────────────────────────────────────────────────────────

/// Kernel, runtime, painter and window rows as one typed report.
#[must_use]
pub fn report() -> phux_perf::PerfReport {
    let mut report = phux_client_core::perf::report(crate::perf::uptime());
    report.metrics.extend(phux_client_runtime::perf::snapshot());
    report
        .metrics
        .extend(crate::perf::TABLE.iter().map(Metric::snapshot));
    report.metrics.extend(TABLE.iter().map(Metric::snapshot));
    report
}

// ── Evidence ───────────────────────────────────────────────────────────

/// `--evidence DIR [--smoke]`.
#[derive(Debug, Clone)]
pub struct EvidenceOptions {
    pub dir: PathBuf,
    /// Quit with status 0 once evidence is written (`--smoke`).
    pub exit_after: bool,
}

/// One painted terminal in a frame the desk is about to present.
#[derive(Debug, Clone)]
pub struct PaintedPane {
    pub placement_id: String,
    pub terminal_id: String,
    /// The publication generation painted.
    pub generation: u64,
    pub bounds: gpui::Bounds<gpui::Pixels>,
    /// Fully inside the window's content mask.
    pub unclipped: bool,
}

/// What a desk painted this frame.
#[derive(Debug, Clone)]
pub struct FrameReport {
    pub session: String,
    pub placements: Vec<(String, String)>,
    pub painted: Vec<PaintedPane>,
}

struct Evidence {
    options: EvidenceOptions,
    written: bool,
}

impl Global for Evidence {}

/// A smoke run wrote its evidence and quit.
static PASSED: AtomicBool = AtomicBool::new(false);
static EVIDENCE_MODE: AtomicBool = AtomicBool::new(false);

/// `PHUX_DESKTOP_BACKGROUND=1`: open windows without taking focus.
static BACKGROUND: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| std::env::var_os("PHUX_DESKTOP_BACKGROUND").is_some());

/// Whether windows may take focus: not in an evidence run or background mode.
#[must_use]
pub fn may_activate() -> bool {
    !EVIDENCE_MODE.load(Ordering::Relaxed) && !*BACKGROUND
}

/// Evidence windows are pop-up level, on every Space including a full-screen
/// one, and never activate: the window server presents them while someone
/// keeps working in another app.
#[must_use]
pub fn window_kind() -> gpui::WindowKind {
    if EVIDENCE_MODE.load(Ordering::Relaxed) {
        gpui::WindowKind::PopUp
    } else {
        gpui::WindowKind::Normal
    }
}

/// Install evidence and perf sampling. Call once, inside `Application::run`.
pub fn install(options: Option<EvidenceOptions>, cx: &mut App) {
    crate::perf::mark_started();
    let Some(options) = options else { return };
    EVIDENCE_MODE.store(true, Ordering::Relaxed);
    if let Err(error) = std::fs::create_dir_all(&options.dir) {
        log::error!("evidence directory {}: {error}", options.dir.display());
    }
    if !options.exit_after {
        spawn_perf_sampler(options.dir.join("perf.jsonl"), cx);
    }
    cx.set_global(Evidence {
        options,
        written: false,
    });
    spawn_evidence_pacer(cx);
}

/// GPUI drops presentation callbacks for a frame drawn while the window is
/// not visible, and a quiet desk does not redraw. Until evidence is written,
/// redraw every window a few times a second so a frame is presented once the
/// window is on screen. Evidence runs only.
fn spawn_evidence_pacer(cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            let pending = cx.update(|cx| {
                if !evidence_pending(cx) {
                    return false;
                }
                for window in cx.windows() {
                    let _ = window.update(cx, |_, window, _| window.refresh());
                }
                true
            });
            if !pending {
                break;
            }
        }
    })
    .detach();
}

/// Whether a desk should build a [`FrameReport`] this frame: evidence is
/// requested and not yet written or armed. Cheap; call every frame.
#[must_use]
pub fn evidence_pending(cx: &App) -> bool {
    cx.try_global::<Evidence>()
        .is_some_and(|evidence| !evidence.written)
}

/// Read what a desk has painted, inside the drawable-reported callback:
/// after this frame's terminals painted, so the evidence names the
/// generation actually on screen.
pub type CollectFrame = Box<dyn FnOnce(&App) -> Option<FrameReport>>;

/// Called by a desk at the end of every render while [`evidence_pending`].
/// Arms a drawable-reported callback for this frame; the first reported
/// frame with a painted terminal writes the evidence, later ones see it
/// written and return.
pub fn arm_evidence(collect: CollectFrame, window: &mut Window, cx: &mut App) {
    if !evidence_pending(cx) {
        return;
    }
    log::trace!(
        "evidence: armed; window visibility {:?}",
        window.native_visibility()
    );
    window.on_drawable_reported(move |window, cx, confirmed| {
        PRESENTED.incr();
        log::debug!("evidence: drawable reported, display confirmed: {confirmed}");
        let visible = window.native_visibility() == gpui::NativeVisibility::Visible;
        let dir = cx.global::<Evidence>().options.dir.clone();
        let painted = collect(cx).and_then(|report| {
            let pane = report
                .painted
                .iter()
                .find(|pane| pane.generation > 0)
                .cloned()?;
            Some((report, pane))
        });
        if !evidence_pending(cx) {
            return;
        }
        let Some((report, pane)) = painted else {
            log::debug!("evidence: presented frame has no painted terminal yet");
            return;
        };
        match write_evidence(&dir, &report, &pane, visible, confirmed, window) {
            Ok(()) => {
                cx.global_mut::<Evidence>().written = true;
                log::info!("evidence written to {}", dir.display());
                if cx.global::<Evidence>().options.exit_after {
                    PASSED.store(true, Ordering::Relaxed);
                    cx.quit();
                }
            }
            Err(error) => log::warn!("evidence capture failed, retrying: {error:#}"),
        }
    });
}

fn write_evidence(
    dir: &Path,
    report: &FrameReport,
    pane: &PaintedPane,
    visible: bool,
    confirmed: bool,
    window: &Window,
) -> anyhow::Result<()> {
    let image = window.render_to_image()?;
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    write_atomic(&dir.join("presented.png"), &png)?;
    let placements: Vec<_> = report
        .placements
        .iter()
        .map(|(id, terminal)| json!({ "id": id, "terminalId": terminal }))
        .collect();
    let layout = json!({
        "session": report.session,
        // The display confirmed a nonzero presented time for this drawable.
        // A virtual display (hosted CI) reports `false` for every drawable.
        "presented": confirmed,
        // The exact drawable was submitted to a visible window and reported.
        "submitted": true,
        "generation": pane.generation,
        "placements": placements,
    });
    write_atomic(&dir.join("layout.json"), layout.to_string().as_bytes())?;
    let interaction = json!({
        "kind": "present",
        "placementId": pane.placement_id,
        "terminalId": pane.terminal_id,
        "presentedGeneration": pane.generation,
        "visible": visible,
        "clipped": !pane.unclipped,
        "bounds": {
            "width": f32::from(pane.bounds.size.width),
            "height": f32::from(pane.bounds.size.height),
        },
    });
    write_atomic(
        &dir.join("interaction.json"),
        interaction.to_string().as_bytes(),
    )
}

fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let staged = path.with_extension("partial");
    std::fs::write(&staged, bytes)?;
    std::fs::rename(&staged, path)?;
    Ok(())
}

// ── perf.jsonl ─────────────────────────────────────────────────────────

fn spawn_perf_sampler(path: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx| {
        let started = Instant::now();
        let mut previous = Sample::capture();
        loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let now = Sample::capture();
            let line = perf_line(started.elapsed(), &now.delta(&previous));
            previous = now;
            let appended = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut file| file.write_all(line.as_bytes()));
            if let Err(error) = appended {
                log::warn!("perf.jsonl: {error}");
            }
        }
    })
    .detach();
}

/// The window-level rows at one instant; deltas give one second of activity.
struct Sample {
    frame: phux_perf::HistogramSnapshot,
    drain: phux_perf::HistogramSnapshot,
    wakes: u64,
    events: u64,
}

impl Sample {
    fn capture() -> Self {
        Self {
            frame: FRAME.snapshot(),
            drain: DRAIN.snapshot(),
            wakes: DRAIN_WAKES.get(),
            events: DRAIN_EVENTS.get(),
        }
    }

    fn delta(&self, previous: &Self) -> Self {
        Self {
            frame: self.frame.delta(&previous.frame),
            drain: self.drain.delta(&previous.drain),
            wakes: self.wakes.saturating_sub(previous.wakes),
            events: self.events.saturating_sub(previous.events),
        }
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "millisecond display of microsecond samples"
)]
fn ms(micros: u64) -> f64 {
    micros as f64 / 1000.0
}

fn perf_line(uptime: Duration, second: &Sample) -> String {
    let host = report();
    let line = json!({
        "at": unix_millis(),
        "uptimeMs": uptime.as_millis(),
        "frames": {
            "frames": second.frame.count,
            "p99Ms": ms(second.frame.percentile(99)),
            "maxMs": ms(second.frame.max),
        },
        "drains": {
            "wakes": second.wakes,
            "events": second.events,
            "ms": ms(second.drain.sum),
            "maxMs": ms(second.drain.max),
        },
        "memory": { "rss": host.process.map(|process| process.max_rss_bytes) },
        "host": host,
    });
    format!("{line}\n")
}

/// The process status once the event loop returns.
#[must_use]
pub fn exit_code() -> std::process::ExitCode {
    // An evidence run that quit before writing evidence did not pass.
    if EVIDENCE_MODE.load(Ordering::Relaxed) && !PASSED.load(Ordering::Relaxed) {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}
