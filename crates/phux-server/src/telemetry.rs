//! Process-global telemetry bootstrap.
//!
//! Wires up `tracing` so that the existing `tracing::{info,debug,warn}!`
//! call sites across the workspace actually emit. Without this, every
//! tracing macro is a silent no-op (no subscriber installed).
//!
//! Two entry points share one layer builder:
//!
//! * [`init`] — the **server / foreground** path. Keeps the long-standing
//!   human-text fmt layer writing to **stderr** (the binary's stdout is
//!   reserved for protocol/PTY traffic — `phux stdio-bridge` splices it
//!   into the wire; never pollute it with log lines). Optionally *also*
//!   tees to a file when
//!   `PHUX_LOG` is set, and installs the `tokio-console` layer when built
//!   for it.
//! * [`init_client`] — the **client / TUI** path. NEVER writes to stderr:
//!   the attach loop owns the alt screen, so a stray log line corrupts the
//!   display. It logs to a file only — `PHUX_LOG` when set, else a
//!   per-pid default under `$XDG_STATE_HOME/phux/` — so a client crash or
//!   warning is always recoverable from disk.
//!
//! Shared environment knobs (read once, at init):
//!
//! * `RUST_LOG` — the filter. Defaults to `phux=info,warn`. Same
//!   precedence for both entry points.
//! * `PHUX_LOG=<path>` — write logs to this file (via a [`tracing_appender`]
//!   writer — non-blocking for the server, synchronous for the client). For
//!   the server this is *in addition to* stderr; for the client it overrides
//!   the per-pid default path.
//! * `PHUX_LOG_FORMAT=text|json` — choose the human fmt layer (`text`,
//!   the default) or a structured JSON fmt layer (one JSON object per
//!   line, `jq`/`grep`-able).
//!
//! Both fmt layers emit span-close timing (`FmtSpan::CLOSE`) so any
//! `#[instrument]` span reports its duration on close — the substrate the
//! lag/crash flywheel reads to find hot paths.
//!
//! [`init`] (server) uses a NON-blocking file writer and returns a
//! [`WorkerGuard`] that `main` must keep alive for the process lifetime;
//! dropping it flushes and stops the background writer thread. [`init_client`]
//! instead uses a SYNCHRONOUS writer and returns no guard: the client exits
//! via `std::process::exit` (which skips guard Drop), so a buffered tail
//! would be lost — synchronous writes have none to lose.
//!
//! Each entry point is **idempotent at the type level only** — call at
//! most once per process. Subsequent calls return `Err` via `try_init`'s
//! error path; callers should not call them from tests.
//!
//! The canonical server log is also **rotated while the server runs**, not
//! only at startup: [`run_log_rotation_task`] is a background task the
//! server binary spawns on its own tokio runtime, which periodically bounds
//! `server.log` at `LOG_ROTATE_THRESHOLD_BYTES` the same way startup
//! rotation already bounds it across many short-lived server generations
//! (phux-j1zj).
//!
//! ## Why factor this out
//!
//! A follow-up agent will add a `dhat-heap` feature that swaps the global
//! allocator. Allocator setup happens *outside* this module (it requires a
//! `#[global_allocator]` static + a `dhat::Profiler` guard owned by
//! `main`), so this module deliberately stays allocator-agnostic and
//! additive.

use std::path::{Path, PathBuf};

/// Re-export so binary crates can name the guard's type (to bind it for
/// the process lifetime) without a direct `tracing-appender` dependency.
/// The guard must outlive the process: dropping it flushes and stops the
/// non-blocking file writer's background thread.
pub use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, fmt};

/// Default tracing filter applied when `RUST_LOG` is unset.
///
/// `phux=info` keeps server-side `info!` lines visible without drowning
/// the operator in `tokio`/`hyper`/etc. The trailing `warn` fallback
/// ensures genuinely surprising events from any crate still surface.
const DEFAULT_FILTER: &str = "phux=info,warn";

/// Environment variable naming an explicit log file path. When set, the
/// server tees logs to it (in addition to stderr) and the client writes
/// to it instead of the per-pid default.
const ENV_LOG_PATH: &str = "PHUX_LOG";

/// Environment variable selecting the on-disk / on-stderr log format:
/// `text` (default, human) or `json` (one JSON object per line).
const ENV_LOG_FORMAT: &str = "PHUX_LOG_FORMAT";

/// Output encoding for a fmt layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogFormat {
    /// Human-readable single-line text (the historical default).
    Text,
    /// One JSON object per line — `jq`/`grep`-able structured logs.
    Json,
}

impl LogFormat {
    /// Resolve the format from `PHUX_LOG_FORMAT`. Unset or unrecognized
    /// values fall back to [`LogFormat::Text`] — logging must never fail
    /// to start over a typo'd env var.
    fn from_env() -> Self {
        match std::env::var(ENV_LOG_FORMAT) {
            Ok(v) if v.eq_ignore_ascii_case("json") => Self::Json,
            _ => Self::Text,
        }
    }
}

/// Build the env filter from `RUST_LOG`, falling back to [`DEFAULT_FILTER`].
fn env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER))
}

/// Build a fmt layer over an arbitrary writer, honoring the requested
/// format and emitting span-close timing.
///
/// Generic over the subscriber `S` (so it composes into any registry) and
/// the writer factory `W` (stderr, a non-blocking file appender, …). Both
/// the text and JSON branches set [`FmtSpan::CLOSE`] so a span reports its
/// elapsed time when it closes — the timing signal the next wave's
/// `#[instrument]` spans rely on.
fn fmt_layer<S, W>(format: LogFormat, writer: W, ansi: bool) -> Box<dyn Layer<S> + Send + Sync>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    W: for<'w> fmt::MakeWriter<'w> + Send + Sync + 'static,
{
    match format {
        LogFormat::Text => fmt::layer()
            .with_writer(writer)
            .with_ansi(ansi)
            .with_span_events(FmtSpan::CLOSE)
            .boxed(),
        LogFormat::Json => fmt::layer()
            .json()
            .with_writer(writer)
            .with_span_events(FmtSpan::CLOSE)
            .boxed(),
    }
}

/// Size at which the log is rolled aside (phux-zomb.5, phux-j1zj).
///
/// Deliberately generous: the log has to be long enough to cover a real
/// debugging session, and the failure this bounds is unbounded growth
/// across *generations* (many short-lived servers appending to one file)
/// as well as within a single very long-lived, chatty run.
const LOG_ROTATE_THRESHOLD_BYTES: u64 = 8 * 1024 * 1024;

/// How many previous generations of a rotated log are kept
/// (`<path>.1` .. `<path>.{LOG_ROTATE_MAX_GENERATIONS}`).
///
/// Bounds total retained history to roughly `LOG_ROTATE_MAX_GENERATIONS *
/// LOG_ROTATE_THRESHOLD_BYTES` on top of the live file, instead of letting
/// `.1`, `.2`, … accumulate without limit — the total is capped, not merely
/// chunked into more, equally-unbounded pieces.
///
/// A constant rather than a config knob: the config schema is inside the
/// ADR-0071 1.0 freeze, and nothing about this number has proven worth
/// exposing yet.
const LOG_ROTATE_MAX_GENERATIONS: usize = 4;

/// How often [`run_log_rotation_task`] re-checks the canonical server log's
/// size while the server is live (phux-j1zj).
///
/// Five minutes keeps the check itself cheap (one `stat`, almost always a
/// no-op) while staying well under the time it would take a single
/// long-lived, chatty server to cross [`LOG_ROTATE_THRESHOLD_BYTES`]
/// unnoticed.
const LOG_ROTATE_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

/// Whether `size` bytes warrants rotating the log.
///
/// Pure and filesystem-free so the rotation *trigger* is testable with
/// synthetic sizes rather than real files or a wall clock.
const fn needs_rotation(size: u64, threshold: u64) -> bool {
    size >= threshold
}

/// The path of the `n`th rotated generation of `base`: `base.1`, `base.2`, …
fn generation_path(base: &Path, n: usize) -> PathBuf {
    let mut path = base.as_os_str().to_owned();
    path.push(format!(".{n}"));
    PathBuf::from(path)
}

/// The renames needed to shift existing rotated generations up one slot
/// before a fresh `.1` is written, in the order they must be applied
/// (highest generation first, so no rename clobbers a file that hasn't
/// moved yet).
///
/// The final rename in the plan (`.{max-1}` -> `.{max}`) overwrites
/// whatever already sits at `.{max}` — `std::fs::rename` replaces an
/// existing destination atomically — which is how the oldest generation is
/// dropped: by that overwrite, not a separate delete. With
/// `max_generations <= 1` there is nothing to shift, so the plan is empty.
///
/// Pure and filesystem-free: the plan depends only on `base` and
/// `max_generations`, so the retention *cap* is testable without touching
/// disk.
fn shift_plan(base: &Path, max_generations: usize) -> Vec<(PathBuf, PathBuf)> {
    (1..max_generations)
        .rev()
        .map(|n| (generation_path(base, n), generation_path(base, n + 1)))
        .collect()
}

/// Roll `path` aside if it has grown past `threshold`, keeping up to
/// `max_generations` previous generations at `path.1` .. `path.{max_generations}`.
///
/// Copies the live file's content into `path.1` (after shifting any
/// existing `.1` .. `.{max_generations - 1}` up one slot, oldest dropped),
/// then **truncates `path` in place** — it never renames or recreates the
/// live path itself. That distinction matters because `path` is a shared,
/// fixed location: a service-managed server's stdio redirect and any
/// reader already following it by name (`tail -f`, `phux logs --server
/// -f`) have it open *before* this runs. An in-place truncate leaves their
/// file descriptor pointing at the same inode, so an `O_APPEND` writer
/// keeps landing at the (now-zero) end of the file and a `tail -f` reader
/// sees the truncation and keeps following — neither has to reopen
/// anything. A rename-based rotation would instead orphan every existing
/// reader or writer on the old inode, silently.
///
/// Called both once at startup (via [`file_writer`], for the optional
/// `PHUX_LOG` tee) and periodically for as long as the server runs (via
/// [`run_log_rotation_task`], for the canonical `server.log`), so a single
/// very long-lived, very chatty server is bounded the same way many
/// short-lived ones already were (phux-j1zj).
///
/// Returns `Ok(true)` if a rotation happened. Callers swallow every `Err`:
/// logging must never be the reason a server refuses to start or stumbles
/// while running.
fn rotate_log(path: &Path, threshold: u64, max_generations: usize) -> std::io::Result<bool> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(false);
    };
    if !needs_rotation(meta.len(), threshold) {
        return Ok(false);
    }
    for (from, to) in shift_plan(path, max_generations) {
        if from.exists() {
            std::fs::rename(&from, &to)?;
        }
    }
    if max_generations > 0 {
        let gen1 = generation_path(path, 1);
        std::fs::copy(path, &gen1)?;
        // `fs::copy` carries the source's permission bits on Unix, but
        // re-harden explicitly (ADR-0028) rather than lean on that being
        // true on every platform forever.
        harden_log_sink(&gen1)?;
    }
    // In-place truncate (not a rename+recreate) — see the doc comment above
    // for why that is load-bearing for readers and writers already holding
    // `path` open by name.
    std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)?;
    Ok(true)
}

/// One rotation check against the canonical server log, with production's
/// threshold and retention cap.
///
/// Synchronous and tokio-free by design: it is the entire body of one
/// [`run_log_rotation_task`] tick, factored out so the "does the canonical
/// path get rotated with the right numbers" wiring is unit-testable
/// directly — set `XDG_STATE_HOME`, seed a file, call this, assert — with
/// no runtime, no background task, and no timing involved.
fn rotate_server_log_if_needed() -> std::io::Result<bool> {
    rotate_log(
        &server_log_path(),
        LOG_ROTATE_THRESHOLD_BYTES,
        LOG_ROTATE_MAX_GENERATIONS,
    )
}

/// Periodically rotate the canonical `server.log` for as long as the
/// server is live (phux-j1zj).
///
/// Startup-only rotation (the check inside `file_writer`) bounds growth
/// across many short-lived server generations, but a single very
/// long-lived, chatty server could still cross `LOG_ROTATE_THRESHOLD_BYTES`
/// within one run and never get rolled aside. This task closes that gap:
/// spawn it once, on the server's own tokio runtime, and let it run until
/// the runtime is dropped at shutdown.
///
/// `tokio::time::interval` fires its first tick immediately, so a log that
/// was already oversized when this server started gets bounded right away
/// rather than after a full `LOG_ROTATE_CHECK_INTERVAL` — on top of,
/// not instead of, whatever startup-time rotation an explicit `PHUX_LOG`
/// path already received from [`init`].
///
/// The actual check runs via `spawn_blocking`: almost every tick it is one
/// cheap `stat`, but on the rare oversized tick it becomes a multi-MiB file
/// copy, which must not run inline on a current-thread runtime's single
/// reactor thread (ADR-0003) — that would stall every pane's PTY I/O and
/// every socket accept for the duration of the copy.
pub async fn run_log_rotation_task() {
    let mut ticker = tokio::time::interval(LOG_ROTATE_CHECK_INTERVAL);
    loop {
        ticker.tick().await;
        let outcome = tokio::task::spawn_blocking(rotate_server_log_if_needed).await;
        if let Ok(Err(err)) = outcome {
            tracing::debug!(error = %err, "server log rotation check failed");
        }
        // A panicked join (`Err` from `spawn_blocking`) is swallowed the
        // same as an `Err` from the rotation itself: a broken rotation
        // check is never a reason to bring the server down.
    }
}

/// Open a non-blocking file appender at `path`, creating the parent
/// directory if needed.
///
/// Returns the [`WorkerGuard`] (which must outlive the process to keep the
/// background writer alive) alongside a `MakeWriter` factory. We use a
/// fixed file name rather than a daily-rolling one so a `PHUX_LOG` path the
/// operator names points at exactly that file; size-based rotation happens
/// here at startup ([`rotate_log`]), and — for the canonical server log —
/// again periodically for as long as the server runs
/// ([`run_log_rotation_task`]).
fn file_writer(
    path: &Path,
) -> std::io::Result<(tracing_appender::non_blocking::NonBlocking, WorkerGuard)> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = rotate_log(path, LOG_ROTATE_THRESHOLD_BYTES, LOG_ROTATE_MAX_GENERATIONS);
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::other(format!(
            "PHUX_LOG path has no file name: {}",
            path.display()
        ))
    })?;
    // Create the sink at mode 0o600 BEFORE the appender opens it (ADR-0028):
    // logs carry self-narrating input atoms and timing detail, so the file
    // must not be world- or group-readable. `rolling::never` appends with
    // `OpenOptions::create(true).append(true)`, whose default mode is 0o644 —
    // pre-creating (or re-chmod-ing) the file makes the append a no-op on perms
    // and leaves the sink user-only.
    harden_log_sink(path)?;
    // `tracing_appender::rolling::never` is the non-rotating file sink: it
    // appends to exactly `dir/file_name`. A bare path (no directory) logs
    // into the current directory.
    let appender = tracing_appender::rolling::never(
        dir.map_or_else(|| PathBuf::from("."), Path::to_path_buf),
        file_name,
    );
    Ok(tracing_appender::non_blocking(appender))
}

/// Ensure the log sink at `path` exists and is owner-only (mode `0o600`)
/// before any appender writes to it (ADR-0028).
///
/// Log files capture redaction-safe-but-still-sensitive operational detail
/// (input-atom narration, span timing, panics); on a shared multi-user box
/// they must not be readable by other users. The default file-creation mode
/// (`0o644`) is group/world-readable, so we create the file ourselves with the
/// tight mode and re-tighten an existing file's perms. No-op on non-Unix
/// targets, where file modes don't apply.
fn harden_log_sink(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        // Create (if absent) with 0o600 in one atomic step, so the file is
        // never briefly group/world-readable.
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        // If it already existed with looser perms (e.g. created before this
        // hardening, or by another tool), tighten it now.
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Per-pid default client log path: `$XDG_STATE_HOME/phux/client-<pid>.log`
/// (falling back to `$HOME/.local/state/phux/` when `XDG_STATE_HOME` is
/// unset, matching the XDG base-directory default).
///
/// Pid-scoping keeps concurrent clients from interleaving into one file
/// and makes "which log is this crash in" answerable from the client's
/// own pid. Public so a future `phux` subcommand (or a test) can report
/// the path it would use.
#[must_use]
pub fn default_client_log_path() -> PathBuf {
    let mut dir = client_state_dir();
    dir.push(format!("client-{}.log", std::process::id()));
    dir
}

/// Canonical server log path: `$XDG_STATE_HOME/phux/server.log` (falling
/// back to `$HOME/.local/state/phux/` when `XDG_STATE_HOME` is unset,
/// matching the XDG base-directory default).
///
/// The ONE server log, regardless of how the server was started: the
/// auto-spawn path redirects the daemon's stderr here, and the
/// service-install unit points its log capture at the same file. Every
/// consumer that names or tails "the server log" (`phux service logs`,
/// doctor/log-inventory verbs) must resolve it through this helper so the
/// writers and the readers can never disagree about the path
/// (phux-i0e8.5.1).
#[must_use]
pub fn server_log_path() -> PathBuf {
    state_dir().join("server.log")
}

/// phux's per-user, per-profile state directory.
///
/// `$XDG_STATE_HOME/phux` (or `$HOME/.local/state/phux` when `XDG_STATE_HOME`
/// is unset/empty), suffixed with the active profile when it is not the
/// default one.
///
/// The home for state that should survive across runs but isn't config: the
/// canonical server log ([`server_log_path`]), client logs (per-pid), and the
/// auto-provisioned remote-consumer TLS cert + token store (ADR-0031).
///
/// Profile-scoped via [`phux_config::instance::state_dir`] so a development
/// build's logs and provisioned credentials cannot be confused with — or
/// written over — those of the installed build (phux-zomb.2).
#[must_use]
pub fn state_dir() -> PathBuf {
    phux_config::instance::state_dir()
}

/// `$XDG_STATE_HOME/phux` (or `$HOME/.local/state/phux`).
fn client_state_dir() -> PathBuf {
    state_dir()
}

/// Install the process-global `tracing` subscriber for a **server /
/// foreground** process.
///
/// Call this from the binary entry point **before** building the tokio
/// runtime. Calling it (or [`init_client`]) more than once will return
/// `Err`.
///
/// Always installs the historical human-or-JSON fmt layer to **stderr**.
/// When `PHUX_LOG` is set it *also* tees the same-format stream to that
/// file via a non-blocking writer; the returned [`WorkerGuard`] (when
/// present) must be held for the process lifetime so the file writer keeps
/// flushing.
///
/// Installs **no panic hook**. This is the subscriber for every
/// non-TUI process — a foreground server, yes, but also every one-shot CLI
/// verb — and [`install_server_panic_hook`] labels its event `server panic`.
/// A CLI that died writing to a closed pipe used to log exactly that,
/// pointing triage at a server that was fine (phux-h5hj.8, phux-ngq2). The
/// hook belongs to the long-running daemons, so they arm it themselves:
/// `phux server` and `phux relay run` both call
/// [`install_server_panic_hook`] on entry.
///
/// Returns `Err` if a subscriber was already installed (e.g. by a test
/// harness or a buggy second call); callers in `main` can treat this as
/// fatal and exit non-zero, or simply log and continue.
pub fn init() -> Result<Option<WorkerGuard>, Box<dyn std::error::Error + Send + Sync>> {
    let format = LogFormat::from_env();

    // Always-on stderr layer (ANSI for an interactive operator).
    let stderr_layer = fmt_layer(format, std::io::stderr as fn() -> std::io::Stderr, true);

    // Optional file tee. ANSI is off for files (escape codes would
    // pollute a log a human greps / a tool parses).
    let (file_layer, guard) = match std::env::var_os(ENV_LOG_PATH) {
        Some(path) if !path.is_empty() => {
            let path = PathBuf::from(path);
            let (writer, guard) = file_writer(&path)?;
            (Some(fmt_layer(format, writer, false)), Some(guard))
        }
        _ => (None, None),
    };

    let registry = tracing_subscriber::registry()
        .with(env_filter())
        .with(stderr_layer)
        .with(file_layer);

    // The `tokio-console` integration is purely additive: it adds a
    // second layer that publishes runtime task instrumentation to a gRPC
    // server (default 127.0.0.1:6669) that the `tokio-console` CLI
    // connects to. `console_subscriber::spawn()` PANICS unless Tokio was
    // built with `--cfg tokio_unstable`, so we gate on the cfg too (not
    // the feature alone) — otherwise a `--all-features` build would
    // produce a binary that aborts on startup. The `tokio_unstable` cfg
    // name is declared expected in this crate's build.rs.
    #[cfg(all(feature = "tokio-console", tokio_unstable))]
    {
        let console_layer = console_subscriber::ConsoleLayer::builder()
            .with_default_env()
            .spawn();
        registry.with(console_layer).try_init()?;
    }

    #[cfg(not(all(feature = "tokio-console", tokio_unstable)))]
    {
        registry.try_init()?;
    }

    Ok(guard)
}

/// Install the process-global `tracing` subscriber for a **client / TUI**
/// process.
///
/// Logs to a **file only** — never stdout/stderr — because the attach loop
/// owns the alt screen and any stray write corrupts the display. The sink
/// is `PHUX_LOG` when set, else [`default_client_log_path`]
/// (`$XDG_STATE_HOME/phux/client-<pid>.log`). Honors `PHUX_LOG_FORMAT` and
/// `RUST_LOG` exactly like [`init`].
///
/// Call this from the client/attach entry **before** raw mode is entered.
/// The returned [`WorkerGuard`] must be held for the process lifetime
/// (bind it in `main`); dropping it flushes and stops the writer thread.
///
/// Does NOT install a panic hook — the client's terminal-restoring panic
/// hook in `attach::driver` chains the panic-to-log behavior itself, so
/// that the log write happens before the terminal is restored.
///
/// Returns `Err` if a subscriber was already installed or the log file
/// could not be opened.
pub fn init_client() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let format = LogFormat::from_env();
    let path = std::env::var_os(ENV_LOG_PATH)
        .filter(|v| !v.is_empty())
        .map_or_else(default_client_log_path, PathBuf::from);

    // BLOCKING (synchronous) writer, NOT the server's non-blocking appender.
    // The client leaves its detach/signal paths via `std::process::exit`,
    // which skips a `WorkerGuard`'s flush-on-Drop and would silently drop the
    // buffered trace tail — exactly when you detach right after reproducing a
    // lag/crash. A synchronous appender has no buffered tail to lose, so no
    // guard is needed; the client log path is not latency-critical.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::other(format!(
            "PHUX_LOG path has no file name: {}",
            path.display()
        ))
    })?;
    // Create the client log at mode 0o600 before the appender opens it
    // (ADR-0028); see `harden_log_sink`.
    harden_log_sink(&path)?;
    let appender = tracing_appender::rolling::never(
        dir.map_or_else(|| PathBuf::from("."), Path::to_path_buf),
        file_name,
    );
    let file_layer = fmt_layer(format, appender, false);

    tracing_subscriber::registry()
        .with(env_filter())
        .with(file_layer)
        .try_init()?;

    Ok(())
}

/// Whether the server panic hook has already been installed. The hook is
/// process-global; a re-entrant install would chain it indefinitely.
static SERVER_PANIC_HOOK_INSTALLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Install a global panic hook that logs the panic message + a captured
/// backtrace through `tracing` (so a daemonized server's crash is durable
/// in the log file), then chains the previous hook.
///
/// Idempotent — repeated calls after the first are no-ops.
///
/// Call this from a **long-running daemon's** entry point only, after
/// [`init`] — `phux server` and `phux relay run` do. It is deliberately NOT
/// armed by [`init`]: that would also arm it for every one-shot CLI verb,
/// whose panics would then be reported as `server panic` from a process
/// that is not a server (phux-h5hj.8). A CLI verb's panic still reaches the
/// user through the default hook on stderr; nothing in a one-shot verb
/// needs a durable crash record, because the operator is standing right
/// there reading it.
///
/// The backtrace honors `RUST_BACKTRACE` like the default hook: an
/// unforced [`std::backtrace::Backtrace::capture`] is `Disabled` (and
/// renders as a hint to set `RUST_BACKTRACE=1`) unless the env var is set,
/// so we don't pay the symbolication cost in the common no-crash-config
/// case while still capturing a full trace when the operator asks for one.
pub fn install_server_panic_hook() {
    use std::sync::atomic::Ordering;
    if SERVER_PANIC_HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::capture();
        let location = info
            .location()
            .map_or_else(|| "<unknown>".to_owned(), ToString::to_string);
        tracing::error!(
            panic.location = %location,
            panic.message = %info,
            panic.backtrace = %backtrace,
            "server panic",
        );
        previous(info);
    }));
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    /// `PHUX_LOG_FORMAT=json` (any case) selects JSON; anything else —
    /// including unset — is text.
    #[test]
    fn log_format_from_env_parses_json_case_insensitively() {
        // SAFETY-NOTE: env mutation is process-global; this test runs
        // serially within the module and restores the var.
        let prev = std::env::var_os(ENV_LOG_FORMAT);
        // Safe in test context: single-threaded within this unit and we
        // restore below. `set_var`/`remove_var` are unsafe in edition
        // 2024; the harness owns the process here.
        unsafe { std::env::set_var(ENV_LOG_FORMAT, "JSON") };
        assert_eq!(LogFormat::from_env(), LogFormat::Json);
        unsafe { std::env::set_var(ENV_LOG_FORMAT, "text") };
        assert_eq!(LogFormat::from_env(), LogFormat::Text);
        unsafe { std::env::remove_var(ENV_LOG_FORMAT) };
        assert_eq!(LogFormat::from_env(), LogFormat::Text);
        match prev {
            Some(v) => unsafe { std::env::set_var(ENV_LOG_FORMAT, v) },
            None => unsafe { std::env::remove_var(ENV_LOG_FORMAT) },
        }
    }

    /// `server_log_path` honors `XDG_STATE_HOME` and always names
    /// `<profile-dir>/server.log` under it — the single path both spawn
    /// paths write and every reader tails (phux-i0e8.5.1).
    ///
    /// The directory carries the active profile (ADR-0080), which this
    /// debug-built test binary resolves to `dev`; it is read from
    /// `instance::state_dir` rather than hardcoded so the assertion stays
    /// true under any profile.
    #[test]
    fn server_log_path_honors_xdg_state_home() {
        let prev = std::env::var_os("XDG_STATE_HOME");
        let leaf = phux_config::instance::state_dir()
            .file_name()
            .expect("the state dir always has a final component")
            .to_string_lossy()
            .into_owned();
        // SAFETY-NOTE: env mutation is process-global; nextest runs each
        // test in its own process, and we restore the var below anyway.
        // `set_var`/`remove_var` are unsafe in edition 2024; the harness
        // owns the process here.
        unsafe { std::env::set_var("XDG_STATE_HOME", "/custom/state") };
        assert_eq!(
            server_log_path(),
            PathBuf::from(format!("/custom/state/{leaf}/server.log"))
        );
        // Unset (and empty, which must behave as unset) falls back to
        // `$HOME/.local/state`.
        unsafe { std::env::set_var("XDG_STATE_HOME", "") };
        let fallback = server_log_path();
        assert!(
            fallback.ends_with(format!(".local/state/{leaf}/server.log")),
            "got {fallback:?}"
        );
        match prev {
            Some(v) => unsafe { std::env::set_var("XDG_STATE_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_STATE_HOME") },
        }
    }

    /// The per-pid default client path lives under the phux state dir and
    /// names a `client-<pid>.log` file.
    #[test]
    fn default_client_log_path_is_pid_scoped_under_state_dir() {
        let path = default_client_log_path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("file name");
        assert!(name.starts_with("client-"), "got {name}");
        assert_eq!(
            path.extension().and_then(|e| e.to_str()),
            Some("log"),
            "got {name}"
        );
        assert!(name.contains(&std::process::id().to_string()), "got {name}");
        assert!(path.to_string_lossy().contains("phux"), "got {path:?}");
    }

    /// The file writer creates the parent directory and the sink file,
    /// and a line written through it is flushed to disk once the guard is
    /// dropped. Exercises the `PHUX_LOG`-points-at-a-file contract from a
    /// unit (no global subscriber install needed).
    #[test]
    fn file_writer_creates_dir_and_writes_a_parseable_line() {
        use std::io::Write as _;
        use tracing_subscriber::fmt::MakeWriter as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("phux-test.log");
        {
            let (writer, _guard) = file_writer(&path).expect("file writer");
            let mut w = writer.make_writer();
            writeln!(w, "{{\"hello\":\"world\"}}").expect("write");
            // _guard drops here, flushing the background writer.
        }
        let contents = std::fs::read_to_string(&path).expect("read back log");
        assert!(contents.contains("hello"), "got: {contents}");
        // Each line is valid JSON (the JSON-format contract).
        let line = contents.lines().next().expect("a line");
        let parsed: serde_json::Value = serde_json::from_str(line).expect("valid JSON line");
        assert_eq!(parsed["hello"], "world");
    }

    /// The file sink is created owner-only (mode `0o600`) — logs carry
    /// operational detail that must not be group/world-readable on a shared
    /// box (ADR-0028). Also verifies an already-existing looser file is
    /// re-tightened.
    #[cfg(unix)]
    #[test]
    fn file_writer_creates_sink_with_0o600_perms() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");

        // Fresh file: created at 0o600.
        let fresh = dir.path().join("fresh.log");
        let (_w, _g) = file_writer(&fresh).expect("file writer");
        let mode = std::fs::metadata(&fresh)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "fresh sink mode was {mode:o}");

        // Pre-existing world-readable file: re-tightened to 0o600.
        let loose = dir.path().join("loose.log");
        std::fs::write(&loose, b"old line\n").expect("seed file");
        std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o644))
            .expect("set loose perms");
        let (_w2, _g2) = file_writer(&loose).expect("file writer");
        let mode = std::fs::metadata(&loose)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "re-hardened sink mode was {mode:o}");
    }

    // -----------------------------------------------------------------
    // live rotation (phux-j1zj)
    // -----------------------------------------------------------------

    /// The rotation trigger is a pure `>=` comparison against the
    /// threshold, exercised with synthetic sizes — no real file and no
    /// wall clock involved.
    #[test]
    fn needs_rotation_triggers_at_and_above_threshold_only() {
        assert!(!needs_rotation(7, 8));
        assert!(needs_rotation(8, 8));
        assert!(needs_rotation(9, 8));
    }

    /// The shift plan orders the highest existing generation first (so an
    /// applied rename never clobbers a file that hasn't moved yet) and
    /// stops one short of `max_generations` — the final rename in the
    /// plan is the one whose *destination* is the cap, so applying the
    /// plan in order both shifts every kept generation up and drops the
    /// oldest one (by overwrite) in a single pass. Pure path arithmetic,
    /// no filesystem.
    #[test]
    fn shift_plan_orders_highest_generation_first_within_the_cap() {
        let base = Path::new("/state/phux/server.log");
        let plan = shift_plan(base, 4);
        assert_eq!(
            plan,
            vec![
                (generation_path(base, 3), generation_path(base, 4)),
                (generation_path(base, 2), generation_path(base, 3)),
                (generation_path(base, 1), generation_path(base, 2)),
            ]
        );
    }

    /// Keeping at most one generation (or zero) means there is nothing to
    /// shift — `.1` is always written fresh by `rotate_log` itself.
    #[test]
    fn shift_plan_is_empty_when_at_most_one_generation_is_kept() {
        let base = Path::new("/state/phux/server.log");
        assert!(shift_plan(base, 1).is_empty());
        assert!(shift_plan(base, 0).is_empty());
    }

    /// Below the threshold, `rotate_log` leaves everything untouched.
    #[test]
    fn rotate_log_below_threshold_is_a_noop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.log");
        std::fs::write(&path, b"small\n").expect("seed");

        let rotated = rotate_log(&path, 1024, 4).expect("rotate check");

        assert!(!rotated);
        assert!(!generation_path(&path, 1).exists());
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "small\n");
    }

    /// Over the threshold, `rotate_log` copies the live content into `.1`
    /// and truncates the live path to empty — the size-based trigger and
    /// the actual rotation, driven end to end (not just the pure
    /// decision function above).
    #[test]
    fn rotate_log_rotates_an_oversized_file_into_generation_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.log");
        std::fs::write(&path, b"a line of pre-rotation content\n").expect("seed");

        let rotated = rotate_log(&path, 4, 4).expect("rotate");

        assert!(rotated);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read live path after rotation"),
            "",
            "live path should be truncated to empty"
        );
        assert_eq!(
            std::fs::read_to_string(generation_path(&path, 1)).expect("read .1"),
            "a line of pre-rotation content\n"
        );
    }

    /// Rotation truncates the live path IN PLACE rather than renaming it
    /// aside — a reader that already has `path` open by name (`tail -f`,
    /// or a service-managed server's OS-redirected stdio, both of which
    /// open it before this ever runs) must keep working through a
    /// rotation without reopening anything. Proven here by asserting the
    /// path's inode is unchanged across the call, and that a handle
    /// opened before rotation observes the truncation directly.
    #[cfg(unix)]
    #[test]
    fn rotate_log_truncates_in_place_so_open_readers_keep_the_same_inode() {
        use std::os::unix::fs::MetadataExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.log");
        std::fs::write(&path, b"a line of pre-rotation content\n").expect("seed");

        // Stand-in for a `tail -f` reader (or the OS-redirected stdio fd a
        // service-managed server writes through): already open by name
        // before rotation happens.
        let reader = std::fs::File::open(&path).expect("open before rotation");
        let ino_before = reader.metadata().expect("metadata").ino();

        let rotated = rotate_log(&path, 4, 4).expect("rotate");
        assert!(rotated);

        let ino_after = std::fs::metadata(&path)
            .expect("metadata after rotation")
            .ino();
        assert_eq!(
            ino_before, ino_after,
            "rotation must truncate the live path in place, not replace its inode"
        );
        // The already-open handle observes the truncation without
        // reopening — it is the same file.
        let via_old_handle = std::fs::read_to_string(&path).expect("read via live path");
        assert_eq!(
            via_old_handle, "",
            "existing reader should see the truncation"
        );
        drop(reader);
    }

    /// Existing generations shift up one slot on each rotation, and the
    /// oldest is dropped once `max_generations` is reached — the total is
    /// capped, not merely chunked into more, equally unbounded pieces.
    #[test]
    fn rotate_log_caps_retained_generations_dropping_the_oldest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.log");
        std::fs::write(&path, b"newest live content\n").expect("seed live");
        let gen1 = generation_path(&path, 1);
        let gen2 = generation_path(&path, 2);
        std::fs::write(&gen1, b"generation one\n").expect("seed .1");
        std::fs::write(&gen2, b"generation two (oldest, should be dropped)\n").expect("seed .2");

        let rotated = rotate_log(&path, 1, 2).expect("rotate");

        assert!(rotated);
        assert_eq!(
            std::fs::read_to_string(&gen2).expect("read .2"),
            "generation one\n",
            ".2 should now hold what was in .1"
        );
        assert_eq!(
            std::fs::read_to_string(&gen1).expect("read .1"),
            "newest live content\n",
            ".1 should now hold the just-rotated live content"
        );
        assert!(
            !generation_path(&path, 3).exists(),
            "max_generations=2 must never produce a .3"
        );
    }

    /// The rotated-aside generation is created owner-only (mode `0o600`),
    /// same as the live file it was copied from (ADR-0028) — rotation must
    /// not loosen a log's permissions.
    #[cfg(unix)]
    #[test]
    fn rotate_log_preserves_0o600_on_the_live_file_and_the_rotated_generation() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.log");
        std::fs::write(&path, b"oversized content to force rotation\n").expect("seed");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");

        rotate_log(&path, 4, 4).expect("rotate");

        let live_mode = std::fs::metadata(&path)
            .expect("live metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(live_mode, 0o600, "live file mode was {live_mode:o}");
        let gen1_mode = std::fs::metadata(generation_path(&path, 1))
            .expect(".1 metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(gen1_mode, 0o600, ".1 mode was {gen1_mode:o}");
    }

    /// The wiring `run_log_rotation_task` calls on every tick —
    /// `rotate_server_log_if_needed` — resolves the *real* canonical path
    /// (via `XDG_STATE_HOME`) and rotates it with production's threshold
    /// and retention cap. Entirely synchronous: no tokio runtime, no
    /// background task, and nothing timing-dependent — the async task
    /// itself is a thin, untested wrapper around this (interval scheduling
    /// is tokio's contract, not this crate's logic to re-test).
    #[test]
    fn rotate_server_log_if_needed_rotates_the_canonical_path_when_oversized() {
        let dir = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("XDG_STATE_HOME");
        // SAFETY-NOTE: env mutation is process-global; nextest runs each
        // test in its own process, and we restore the var below. `set_var`
        // is unsafe in edition 2024; the harness owns the process here.
        unsafe { std::env::set_var("XDG_STATE_HOME", dir.path()) };

        let path = server_log_path();
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let oversized_len = usize::try_from(LOG_ROTATE_THRESHOLD_BYTES + 1).expect("fits usize");
        std::fs::write(&path, vec![b'x'; oversized_len]).expect("seed an already-oversized log");

        let rotated = rotate_server_log_if_needed().expect("rotation check");

        assert!(rotated, "an oversized canonical log should have rotated");
        assert!(generation_path(&path, 1).exists());
        assert_eq!(
            std::fs::read_to_string(&path).expect("live path after rotation"),
            "",
            "live path should be truncated after rotation"
        );

        match prev {
            Some(v) => unsafe { std::env::set_var("XDG_STATE_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_STATE_HOME") },
        }
    }

    /// A panic routed through the hook's tracing call writes the panic
    /// message AND a backtrace field to the configured file sink.
    ///
    /// We exercise the durable-capture mechanism that both the server hook
    /// ([`install_server_panic_hook`]) and the client hook
    /// (`attach::driver::install_panic_hook_once`) share — capture a
    /// `Backtrace`, then `tracing::error!` the message + backtrace BEFORE
    /// any terminal restore — without mutating the process-global panic
    /// hook (which would race other tests). A scoped subscriber points at
    /// a temp file; we emit the same event the hook emits and assert it
    /// lands on disk, forcing `RUST_BACKTRACE` on so the trace is real.
    #[test]
    fn panic_capture_writes_message_and_backtrace_to_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("client-panic.log");
        {
            let (writer, _guard) = file_writer(&path).expect("file writer");
            let layer = fmt_layer(LogFormat::Json, writer, false);
            let subscriber = tracing_subscriber::registry()
                .with(EnvFilter::new("phux=error"))
                .with(layer);
            tracing::subscriber::with_default(subscriber, || {
                // Force a captured (not Disabled) backtrace for the test.
                // Use quoted field keys (rather than dotted bare keys) to
                // avoid a macro-parse ambiguity; the field names match the
                // hook's so the assertion below mirrors production output.
                let backtrace = std::backtrace::Backtrace::force_capture();
                tracing::error!(
                    "panic.location" = "telemetry.rs:1",
                    "panic.message" = "forced test panic",
                    "panic.backtrace" = %backtrace,
                    "client panic",
                );
            });
            // _guard drops here, flushing the background writer.
        }
        let contents = std::fs::read_to_string(&path).expect("read back log");
        assert!(
            contents.contains("forced test panic"),
            "panic message missing: {contents}"
        );
        assert!(
            contents.contains("client panic"),
            "panic event message missing: {contents}"
        );
        // A valid JSON line carrying the backtrace field.
        let line = contents
            .lines()
            .find(|l| l.contains("forced test panic"))
            .expect("panic line");
        let parsed: serde_json::Value = serde_json::from_str(line).expect("valid JSON line");
        let fields = &parsed["fields"];
        assert!(
            fields["panic.backtrace"].is_string(),
            "backtrace field missing: {parsed}"
        );
    }
}
