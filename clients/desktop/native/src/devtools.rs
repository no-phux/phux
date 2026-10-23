//! Dev telemetry: what Developer mode turns on in the desktop, reachable by
//! a person and by an agent alike.
//!
//! Developer mode is on when **Settings > Developer > Developer mode** is
//! saved on ([`prefs`]), or for one run with `PHUX_DEV=1` or `--dev`. The
//! setting applies live: [`set_dev_mode`] turns heap accounting, the HUD and
//! the socket on or off in every window without a restart.
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
pub mod prefs;
pub mod server;
pub mod socket;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use gpui::App;
use phux_perf::{Metric, PerfReport, ProcessStats};
use serde::Serialize;

pub use server::{ServerCli, ServerPoller};

/// Process-wide devtools state, set once by [`init`].
#[derive(Debug)]
pub struct DevTools {
    /// `PHUX_DEV` or `--dev` holds Developer mode on for this run, whatever
    /// the saved setting says.
    pub forced: bool,
    /// How to reach the server's CLI; `None` when no CLI was found.
    pub cli: Option<ServerCli>,
    /// The server's report for the HUD and the monitor.
    pub poller: ServerPoller,
    socket: Mutex<SocketState>,
    ui: OnceLock<async_channel::Sender<UiRequest>>,
}

impl DevTools {
    fn new(forced: bool, cli: Option<ServerCli>) -> Self {
        Self {
            forced,
            cli,
            poller: ServerPoller::default(),
            socket: Mutex::new(SocketState::Off),
            ui: OnceLock::new(),
        }
    }
}

/// Where the dev socket stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SocketState {
    /// Developer mode is off, or the GPUI side has not started yet.
    #[default]
    Off,
    /// Serving at this path.
    Listening(PathBuf),
    /// It could not start: the reason, for the Developer page.
    Failed(String),
}

static DEVTOOLS: OnceLock<DevTools> = OnceLock::new();
static DEV_MODE: AtomicBool = AtomicBool::new(false);
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

/// Developer mode as it applies: forced by the launch, or saved on.
#[must_use]
pub fn effective(forced: bool, saved: bool) -> bool {
    forced || saved
}

/// Record the mode and the server CLI. Call once, before the event loop.
/// `forced` is [`requested`]; `saved` is the Developer mode setting. In
/// Developer mode this also turns heap accounting on and shows the HUD.
pub fn init(forced: bool, saved: bool, cli: Option<ServerCli>) {
    let _ = DEVTOOLS.set(DevTools::new(forced, cli));
    if !effective(forced, saved) {
        return;
    }
    DEV_MODE.store(true, Relaxed);
    phux_perf::alloc::enable();
    HUD_VISIBLE.store(true, Relaxed);
    log::info!(
        "developer mode on ({}): heap accounting {}, HUD, dev socket",
        if forced {
            "PHUX_DEV or --dev"
        } else {
            "setting"
        },
        if phux_perf::alloc::installed() {
            "installed"
        } else {
            "not installed"
        }
    );
}

/// The state [`init`] recorded, or an inert default in tests and fixtures.
#[must_use]
pub fn get() -> &'static DevTools {
    DEVTOOLS.get_or_init(|| DevTools::new(false, None))
}

/// Is Developer mode on now?
#[must_use]
pub fn dev_mode() -> bool {
    DEV_MODE.load(Relaxed)
}

/// Save Developer mode and apply it to this run: heap accounting, the HUD
/// and the dev socket follow it in every window. `PHUX_DEV` or `--dev` keeps
/// it on for this run whatever is saved.
///
/// # Errors
///
/// The setting could not be saved. It still applies to this run.
pub fn set_dev_mode(saved: bool, cx: &mut App) -> anyhow::Result<()> {
    let result = prefs::save(saved);
    apply_dev_mode(effective(get().forced, saved));
    refresh_windows(cx);
    result
}

fn apply_dev_mode(on: bool) {
    if DEV_MODE.swap(on, Relaxed) == on {
        return;
    }
    if on {
        phux_perf::alloc::enable();
        start_socket();
    } else {
        phux_perf::alloc::disable();
        stop_socket();
    }
    set_hud(on);
    log::info!("developer mode {}", if on { "on" } else { "off" });
}

/// Where the dev socket stands now.
#[must_use]
pub fn socket_state() -> SocketState {
    socket_slot().clone()
}

/// Where the dev socket listens, while it does.
#[must_use]
pub fn socket_path() -> Option<PathBuf> {
    match socket_state() {
        SocketState::Listening(path) => Some(path),
        SocketState::Off | SocketState::Failed(_) => None,
    }
}

fn socket_slot() -> MutexGuard<'static, SocketState> {
    get().socket.lock().unwrap_or_else(PoisonError::into_inner)
}

fn start_socket() {
    let mut slot = socket_slot();
    if matches!(*slot, SocketState::Listening(_)) {
        return;
    }
    let server = get().cli.as_ref().map(ServerCli::socket);
    *slot = match socket::path_for(server).and_then(|path| socket::serve(&path)) {
        Ok(path) => {
            log::info!("dev socket listening at {}", path.display());
            SocketState::Listening(path)
        }
        Err(error) => {
            log::warn!("dev socket did not start: {error:#}");
            SocketState::Failed(format!("{error:#}"))
        }
    };
}

fn stop_socket() {
    let mut slot = socket_slot();
    if let SocketState::Listening(path) = std::mem::take(&mut *slot) {
        socket::stop(&path);
        log::info!("dev socket closed at {}", path.display());
    }
}

/// Start the GPUI side: the UI bridge, the HUD pacer, and in Developer mode
/// the socket. Call once, inside `Application::run`.
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
    if dev_mode() {
        start_socket();
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

/// Remove the dev socket on a clean exit; a killed desktop's socket is
/// pruned by the next `phux perf --desktop` that finds it dead.
pub fn shutdown() {
    if let SocketState::Listening(path) = &*socket_slot() {
        let _ = std::fs::remove_file(path);
    }
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

/// Toggle only this desktop's allocation counters, not Developer mode, socket
/// publication or the server. Counts begin now; this is intentionally unsaved.
pub fn set_allocation_counting(enabled: bool, cx: &mut App) {
    if enabled {
        phux_perf::alloc::enable();
    } else {
        phux_perf::alloc::disable();
    }
    refresh_windows(cx);
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
    pub physical_footprint_bytes: Option<u64>,
    pub native_heap: Option<phux_perf::alloc::native::HeapStats>,
}

/// Take a [`Snapshot`] now.
#[must_use]
pub fn snapshot() -> Snapshot {
    // `diag::report` starts from the kernel's report and keeps its role;
    // to a reader of the dev socket this whole process is the desktop.
    let mut report = crate::diag::report();
    report.role = "desktop".to_owned();
    Snapshot {
        report,
        frames: frames::last_second(),
        hud: hud_visible(),
        dev: dev_mode(),
        socket: socket_path(),
        physical_footprint_bytes: ProcessStats::physical_footprint_bytes(),
        native_heap: phux_perf::alloc::native::stats(),
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
    check_trace_server(with_server)?;
    // Arm here first: a refused request must not start a server trace too.
    if !phux_perf::trace::start(phux_perf::trace::DEFAULT_MAX_EVENTS) {
        anyhow::bail!("a trace is already recording in the desktop");
    }
    let server = with_server.then(|| get().cli.clone()).flatten().map(|cli| {
        let seconds = format!("{:.3}", duration.as_secs_f64());
        std::thread::spawn(move || server_trace(&cli, &seconds))
    });
    let started = Instant::now();
    while started.elapsed() < duration {
        sample_counters();
        std::thread::sleep(COUNTER_INTERVAL.min(duration.saturating_sub(started.elapsed())));
    }
    sample_counters();
    let capture = phux_perf::trace::stop()
        .ok_or_else(|| anyhow::anyhow!("the desktop trace was stopped by someone else"))?;
    let dir = trace_dir();
    let (desktop, desktop_json) = write_desktop_trace(&dir, &capture)?;
    let (mut server, mut server_error) = join_server_trace(server, with_server);
    let path = merge_server_trace(
        &dir,
        &desktop,
        &desktop_json,
        &mut server,
        &mut server_error,
    )?;
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

fn check_trace_server(with_server: bool) -> anyhow::Result<()> {
    if !with_server {
        return Ok(());
    }
    if let Some(cli) = &get().cli {
        cli.check_socket()?;
    }
    Ok(())
}

fn write_desktop_trace(
    dir: &Path,
    capture: &phux_perf::trace::Capture,
) -> anyhow::Result<(PathBuf, String)> {
    std::fs::create_dir_all(dir)?;
    let json = capture.to_chrome_json("phux desktop", &tables());
    let (path, mut file) = phux_perf::trace::create_trace_file(dir, "desktop")?;
    std::io::Write::write_all(&mut file, json.as_bytes())?;
    Ok((path, json))
}

fn join_server_trace(
    server: Option<std::thread::JoinHandle<anyhow::Result<PathBuf>>>,
    with_server: bool,
) -> (Option<PathBuf>, Option<String>) {
    match server.map(std::thread::JoinHandle::join) {
        None if with_server => (
            None,
            Some("no phux CLI is known to this desktop".to_owned()),
        ),
        None => (None, None),
        Some(Ok(Ok(path))) => (Some(path), None),
        Some(Ok(Err(error))) => (None, Some(format!("{error:#}"))),
        Some(Err(_)) => (None, Some("the server trace thread panicked".to_owned())),
    }
}

fn merge_server_trace(
    dir: &Path,
    desktop: &Path,
    desktop_json: &str,
    server: &mut Option<PathBuf>,
    server_error: &mut Option<String>,
) -> anyhow::Result<PathBuf> {
    // The server names a path on its own host; over a forwarded socket it
    // is not readable here. That loses the merge, not the desktop's trace.
    let server_json = server.as_ref().map(std::fs::read_to_string);
    let path = match server_json {
        Some(Ok(server_json)) => {
            let merged = phux_perf::trace::merge_chrome_json([desktop_json, server_json.as_str()]);
            let (path, mut file) = phux_perf::trace::create_trace_file(dir, "phux")?;
            std::io::Write::write_all(&mut file, merged.as_bytes())?;
            path
        }
        Some(Err(error)) => {
            *server_error = Some(format!(
                "the server wrote {} but it is not readable here: {error}",
                server.take().unwrap_or_default().display()
            ));
            desktop.to_path_buf()
        }
        None => desktop.to_path_buf(),
    };
    Ok(path)
}

/// `phux perf trace --seconds S --json` against the desktop's server.
fn server_trace(cli: &ServerCli, seconds: &str) -> anyhow::Result<PathBuf> {
    let timeout = server::REPORT_TIMEOUT
        + Duration::from_secs_f64(seconds.parse::<f64>().unwrap_or(0.0).max(0.0) + 60.0);
    let stdout = cli.run(&["perf", "trace", "--seconds", seconds, "--json"], timeout)?;
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
    if let Some(footprint) = ProcessStats::physical_footprint_bytes() {
        phux_perf::trace::counter("memory.physical_footprint_bytes", footprint as f64);
    }
    if let Some(heap) = phux_perf::alloc::native::stats() {
        phux_perf::trace::counter("memory.native_heap_in_use_bytes", heap.in_use_bytes as f64);
        phux_perf::trace::counter(
            "memory.native_heap_capacity_bytes",
            heap.capacity_bytes as f64,
        );
    }
    let frames = frames::recent(Duration::from_millis(250));
    phux_perf::trace::counter("frames.fps", frames.len() as f64 * 4.0);
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
