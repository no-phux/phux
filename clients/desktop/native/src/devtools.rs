//! Dev telemetry: what `PHUX_DEV=1` (or `--dev`) turns on in the desktop,
//! reachable by a person and by an agent alike.
//!
//! - **Frames.** [`frames`] keeps the last ten seconds of desk renders, for
//!   FPS, build percentiles, jank and the HUD sparkline. Always on.
//! - **Heap.** The binary installs [`phux_perf::alloc::Counting`]; dev mode
//!   enables it, so every report carries live and peak heap.
//! - **HUD.** An in-window overlay ([`crate::app::hud`]): FPS, frame build
//!   time, CPU, memory, heap and the server's CPU and memory. On by default
//!   in dev mode; `Toggle Performance HUD` in the palette anywhere.
//! - **Traces.** [`capture_trace`] arms [`phux_perf::trace`] for a few
//!   seconds, samples memory and FPS counter tracks alongside, and writes a
//!   Perfetto file; with the server, `phux perf trace` runs at the same time
//!   and both land on one timeline.
//! - **Socket.** [`socket`] serves all of it as line JSON on
//!   `desktop-<pid>.dev.sock` next to the server socket, which is where
//!   `phux perf --desktop` looks.

pub mod frames;
pub mod server;
pub mod socket;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::App;
use phux_perf::{Metric, PerfReport, ProcessStats};
use serde::Serialize;

pub use server::{ServerCli, ServerPoller};

/// Process-wide devtools state, set once by [`init`].
#[derive(Debug)]
pub struct DevTools {
    /// Dev mode is on.
    pub dev: bool,
    /// How to reach the server's CLI; `None` when no CLI was found.
    pub cli: Option<ServerCli>,
    /// The server's report for the HUD and the monitor.
    pub poller: ServerPoller,
    /// Where the dev socket listens, once it does.
    pub socket: OnceLock<PathBuf>,
    ui: OnceLock<async_channel::Sender<UiRequest>>,
}

static DEVTOOLS: OnceLock<DevTools> = OnceLock::new();
static HUD_VISIBLE: AtomicBool = AtomicBool::new(false);

/// A request the dev socket thread makes of the GPUI thread.
#[derive(Debug, Clone, Copy)]
enum UiRequest {
    /// Repaint every window, for a HUD change.
    Refresh,
}

/// Is dev mode requested by the command line or the environment?
#[must_use]
pub fn requested(flag: bool) -> bool {
    flag || phux_perf::dev::requested()
}

/// Record the mode and the server CLI. Call once, before the event loop.
/// In dev mode this also turns heap accounting on and shows the HUD.
pub fn init(dev: bool, cli: Option<ServerCli>) {
    if dev {
        phux_perf::alloc::enable();
        HUD_VISIBLE.store(true, Relaxed);
        log::info!(
            "dev mode on: heap accounting {}, HUD, dev socket",
            if phux_perf::alloc::installed() {
                "installed"
            } else {
                "not installed"
            }
        );
    }
    let _ = DEVTOOLS.set(DevTools {
        dev,
        cli,
        poller: ServerPoller::default(),
        socket: OnceLock::new(),
        ui: OnceLock::new(),
    });
}

/// The state [`init`] recorded, or an inert default in tests and fixtures.
#[must_use]
pub fn get() -> &'static DevTools {
    DEVTOOLS.get_or_init(|| DevTools {
        dev: false,
        cli: None,
        poller: ServerPoller::default(),
        socket: OnceLock::new(),
        ui: OnceLock::new(),
    })
}

/// Start the GPUI side: the UI bridge, the HUD pacer, and in dev mode the
/// socket. Call once, inside `Application::run`.
pub fn install(cx: &mut App) {
    let (tx, rx) = async_channel::unbounded();
    let devtools = get();
    let _ = devtools.ui.set(tx);
    cx.spawn(async move |cx| {
        while let Ok(UiRequest::Refresh) = rx.recv().await {
            cx.update(|cx| {
                refresh_windows(cx);
                ensure_hud_pacer(cx);
            });
        }
    })
    .detach();
    ensure_hud_pacer(cx);
    if devtools.dev {
        match socket::serve(&socket::path_for(
            devtools.cli.as_ref().map(ServerCli::socket),
        )) {
            Ok(path) => {
                log::info!("dev socket listening at {}", path.display());
                let _ = devtools.socket.set(path);
            }
            Err(error) => log::warn!("dev socket did not start: {error:#}"),
        }
    }
}

fn refresh_windows(cx: &mut App) {
    for window in cx.windows() {
        let _ = window.update(cx, |_, window, _| window.refresh());
    }
}

/// While the HUD shows, repaint twice a second so its readings move on a
/// quiet desk, and keep the server poller watched. The HUD's own cost is
/// those two frames a second; hidden, it costs nothing, not even a timer.
fn ensure_hud_pacer(cx: &mut App) {
    static RUNNING: AtomicBool = AtomicBool::new(false);
    if !hud_visible() || RUNNING.swap(true, Relaxed) {
        return;
    }
    cx.spawn(async move |cx| {
        let _watch = get().cli.as_ref().map(|cli| get().poller.watch(cli));
        while hud_visible() {
            cx.update(refresh_windows);
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
        }
        RUNNING.store(false, Relaxed);
    })
    .detach();
}

/// Is the HUD showing?
#[must_use]
pub fn hud_visible() -> bool {
    HUD_VISIBLE.load(Relaxed)
}

/// Show or hide the HUD in every window, from any thread.
pub fn set_hud(visible: bool) {
    HUD_VISIBLE.store(visible, Relaxed);
    if let Some(tx) = get().ui.get() {
        let _ = tx.try_send(UiRequest::Refresh);
    }
}

/// Every metric table in this process, for naming trace spans.
#[must_use]
pub fn tables() -> [&'static [Metric]; 4] {
    [
        phux_client_core::perf::TABLE,
        phux_client_runtime::perf::TABLE,
        crate::perf::TABLE,
        crate::diag::TABLE,
    ]
}

/// The desktop's full report plus what dev mode adds: the frame summary, CPU
/// is the caller's (it needs an interval).
#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub report: PerfReport,
    pub frames: frames::Summary,
    pub hud: bool,
    pub dev: bool,
    pub socket: Option<PathBuf>,
}

/// Take a [`Snapshot`] now.
#[must_use]
pub fn snapshot() -> Snapshot {
    let devtools = get();
    Snapshot {
        report: crate::diag::report(),
        frames: frames::last_second(),
        hud: hud_visible(),
        dev: devtools.dev,
        socket: devtools.socket.get().cloned(),
    }
}

/// Where trace files go: `PHUX_TRACE_DIR`, else `phux-traces` beside the
/// desktop log.
#[must_use]
pub fn trace_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("PHUX_TRACE_DIR") {
        return PathBuf::from(dir);
    }
    crate::diag::log_dir().map_or_else(
        || std::env::temp_dir().join("phux-traces"),
        |dir| dir.join("phux-traces"),
    )
}

/// What [`capture_trace`] wrote.
#[derive(Debug, Clone, Serialize)]
pub struct TraceResult {
    /// The file to open in <https://ui.perfetto.dev>: the merged trace when
    /// the server took part, else the desktop's.
    pub path: PathBuf,
    /// The desktop's own trace.
    pub desktop: PathBuf,
    /// The server's trace, when it took part.
    pub server: Option<PathBuf>,
    /// Why the server did not take part, when it was asked to.
    pub server_error: Option<String>,
    pub events: usize,
    pub dropped: u64,
    pub duration_ms: u64,
}

/// How often a capture samples its counter tracks.
const COUNTER_INTERVAL: Duration = Duration::from_millis(50);

/// Capture `duration` of spans in this process, and in the server too when
/// `with_server` and a CLI is known. Blocks the calling thread; never call it
/// on the GPUI thread.
///
/// # Errors
///
/// A capture is already armed, or the trace file could not be written.
pub fn capture_trace(duration: Duration, with_server: bool) -> anyhow::Result<TraceResult> {
    let server = with_server.then(|| get().cli.clone()).flatten().map(|cli| {
        let seconds = format!("{:.3}", duration.as_secs_f64());
        std::thread::spawn(move || server_trace(&cli, &seconds))
    });
    if !phux_perf::trace::start(phux_perf::trace::DEFAULT_MAX_EVENTS) {
        anyhow::bail!("a trace is already recording in the desktop");
    }
    let started = Instant::now();
    while started.elapsed() < duration {
        sample_counters();
        std::thread::sleep(COUNTER_INTERVAL.min(duration.saturating_sub(started.elapsed())));
    }
    sample_counters();
    let capture = phux_perf::trace::stop()
        .ok_or_else(|| anyhow::anyhow!("the desktop trace was stopped by someone else"))?;
    let dir = trace_dir();
    std::fs::create_dir_all(&dir)?;
    let stamp = unix_millis();
    let pid = std::process::id();
    let desktop_json = capture.to_chrome_json("phux desktop", &tables());
    let desktop = dir.join(format!("desktop-{stamp}-{pid}.json"));
    std::fs::write(&desktop, &desktop_json)?;
    let (server, server_error) = match server.map(std::thread::JoinHandle::join) {
        None if with_server => (
            None,
            Some("no phux CLI is known to this desktop".to_owned()),
        ),
        None => (None, None),
        Some(Ok(Ok(path))) => (Some(path), None),
        Some(Ok(Err(error))) => (None, Some(format!("{error:#}"))),
        Some(Err(_)) => (None, Some("the server trace thread panicked".to_owned())),
    };
    let path = match &server {
        Some(server_path) => {
            let server_json = std::fs::read_to_string(server_path)?;
            let merged =
                phux_perf::trace::merge_chrome_json([desktop_json.as_str(), server_json.as_str()]);
            let path = dir.join(format!("phux-{stamp}-{pid}.json"));
            std::fs::write(&path, merged)?;
            path
        }
        None => desktop.clone(),
    };
    Ok(TraceResult {
        path,
        desktop,
        server,
        server_error,
        events: capture.event_count(),
        dropped: capture.dropped(),
        duration_ms: u64::try_from(capture.duration().as_millis()).unwrap_or(u64::MAX),
    })
}

/// `phux perf trace --seconds S --json` against the desktop's server.
fn server_trace(cli: &ServerCli, seconds: &str) -> anyhow::Result<PathBuf> {
    let stdout = cli.run(&["perf", "trace", "--seconds", seconds, "--json"])?;
    let value: serde_json::Value = serde_json::from_slice(&stdout)?;
    value["path"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("`phux perf trace` printed no path: {value}"))
}

/// Memory and frame-rate counter tracks for the timeline.
#[allow(
    clippy::cast_precision_loss,
    reason = "byte and frame counts plotted as trace counters"
)]
fn sample_counters() {
    if let Some(rss) = ProcessStats::current_rss_bytes() {
        phux_perf::trace::counter("memory.rss_bytes", rss as f64);
    }
    if let Some(alloc) = phux_perf::alloc::stats() {
        phux_perf::trace::counter("memory.heap_live_bytes", alloc.live_bytes as f64);
    }
    let frames = frames::recent(Duration::from_millis(250));
    phux_perf::trace::counter("frames.fps", frames.len() as f64 * 4.0);
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// `path` without the home directory, for a status line.
#[must_use]
pub fn display_path(path: &Path) -> String {
    std::env::var_os("HOME")
        .and_then(|home| {
            path.strip_prefix(home)
                .ok()
                .map(|rest| format!("~/{}", rest.display()))
        })
        .unwrap_or_else(|| path.display().to_string())
}
