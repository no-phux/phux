//! Process-global `tracing` setup.
//!
//! * [`init`]: server and one-shot CLI. Logs to stderr (stdout carries
//!   protocol/PTY bytes), plus a non-blocking tee to `PHUX_LOG` when set,
//!   plus `tokio-console` when built for it. The returned [`WorkerGuard`]
//!   must live for the process.
//! * [`init_client`]: TUI. Never stderr (the alt screen owns it); a
//!   synchronous file writer at `PHUX_LOG` or a per-pid default, because
//!   the client exits via `std::process::exit` and would lose a buffered
//!   tail.
//!
//! `RUST_LOG` filters (default `phux=info,warn`); `PHUX_LOG_FORMAT` picks
//! `text` or `json`. Both layers report span-close timing. Call either at
//! most once per process. The canonical server log is also rotated while
//! the server runs ([`run_log_rotation_task`]).

use std::path::{Path, PathBuf};

/// Re-exported so binaries can hold the guard without depending on
/// `tracing-appender`; dropping it stops the non-blocking writer.
pub use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, fmt};

/// Default filter when `RUST_LOG` is unset.
const DEFAULT_FILTER: &str = "phux=info,warn";

/// Explicit log file path: a server tee, or the client's file instead of
/// the per-pid default.
const ENV_LOG_PATH: &str = "PHUX_LOG";

/// Log format: `text` (default) or `json`.
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
    /// From `PHUX_LOG_FORMAT`; anything unrecognized is text, so a typo never
    /// stops logging.
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

/// A fmt layer over `writer` in `format`, with span-close timing.
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

/// Size at which a log is rolled aside, bounding growth within one run and
/// across many short-lived servers.
const LOG_ROTATE_THRESHOLD_BYTES: u64 = 8 * 1024 * 1024;

/// Rotated generations kept (`<path>.1` ..), capping total history. A
/// constant because the config schema is frozen (ADR-0071).
const LOG_ROTATE_MAX_GENERATIONS: usize = 4;

/// How often [`run_log_rotation_task`] checks the server log's size.
const LOG_ROTATE_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

/// The path of the `n`th rotated generation of `base`: `base.1`, `base.2`, …
fn generation_path(base: &Path, n: usize) -> PathBuf {
    let mut path = base.as_os_str().to_owned();
    path.push(format!(".{n}"));
    PathBuf::from(path)
}

/// Renames that shift rotated generations up one slot, highest first so
/// nothing is clobbered early. The last rename overwrites `.{max}`, which
/// is how the oldest generation is dropped. Empty for `max_generations <= 1`.
fn shift_plan(base: &Path, max_generations: usize) -> Vec<(PathBuf, PathBuf)> {
    (1..max_generations)
        .rev()
        .map(|n| (generation_path(base, n), generation_path(base, n + 1)))
        .collect()
}

/// Roll `path` aside past `threshold`, keeping up to `max_generations`
/// generations: shift, copy the live file to `.1`, then truncate `path` in
/// place. Truncating (not renaming) keeps existing writers and `tail -f`
/// readers on the same inode. Returns whether it rotated; callers ignore
/// errors, since logging must never stop the server.
fn rotate_log(path: &Path, threshold: u64, max_generations: usize) -> std::io::Result<bool> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(false);
    };
    if meta.len() < threshold {
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
        // Re-harden explicitly (ADR-0028) rather than rely on `fs::copy`.
        harden_log_sink(&gen1)?;
    }
    // In place, not rename + recreate (see above).
    std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)?;
    Ok(true)
}

/// One rotation check of the canonical server log with production limits
/// (the synchronous body of each [`run_log_rotation_task`] tick).
fn rotate_server_log_if_needed() -> std::io::Result<bool> {
    rotate_log(
        &server_log_path(),
        LOG_ROTATE_THRESHOLD_BYTES,
        LOG_ROTATE_MAX_GENERATIONS,
    )
}

/// Periodically rotate the canonical `server.log` while the server runs; spawn
/// once on the server runtime.
///
/// The first tick is immediate. Each check runs in `spawn_blocking`, since a
/// rotation copies megabytes and must not stall the current-thread reactor
/// (ADR-0003).
pub async fn run_log_rotation_task() {
    let mut ticker = tokio::time::interval(LOG_ROTATE_CHECK_INTERVAL);
    loop {
        ticker.tick().await;
        let outcome = tokio::task::spawn_blocking(rotate_server_log_if_needed).await;
        if let Ok(Err(err)) = outcome {
            tracing::debug!(error = %err, "server log rotation check failed");
        }
        // A panicked or failed check is ignored.
    }
}

/// A non-blocking appender at `path` (parent created), plus its
/// [`WorkerGuard`]. Rotates at open.
fn file_writer(
    path: &Path,
) -> std::io::Result<(tracing_appender::non_blocking::NonBlocking, WorkerGuard)> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = rotate_log(path, LOG_ROTATE_THRESHOLD_BYTES, LOG_ROTATE_MAX_GENERATIONS);
    Ok(tracing_appender::non_blocking(appender(path)?))
}

/// An owner-only (ADR-0028) appender to exactly `path`, parent created.
fn appender(path: &Path) -> std::io::Result<tracing_appender::rolling::RollingFileAppender> {
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
    // Pre-create owner-only; the appender's default is 0o644.
    harden_log_sink(path)?;
    Ok(tracing_appender::rolling::never(
        dir.map_or_else(|| PathBuf::from("."), Path::to_path_buf),
        file_name,
    ))
}

/// Ensure the sink exists with mode `0o600` before any write (ADR-0028):
/// logs carry sensitive operational detail. No-op off Unix.
fn harden_log_sink(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        // Create at 0o600 atomically, never briefly readable.
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        // Tighten a pre-existing looser file.
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Per-pid client log path, `<state dir>/client-<pid>.log`.
#[must_use]
pub fn default_client_log_path() -> PathBuf {
    let mut dir = state_dir();
    dir.push(format!("client-{}.log", std::process::id()));
    dir
}

/// The canonical server log, `<state dir>/server.log`, used by every writer
/// and reader of "the server log".
#[must_use]
pub fn server_log_path() -> PathBuf {
    state_dir().join("server.log")
}

/// phux's per-user, per-profile state directory
/// (`$XDG_STATE_HOME/phux`, else `~/.local/state/phux`), holding logs and
/// provisioned credentials; profile-scoped via
/// [`phux_config::instance::state_dir`].
#[must_use]
pub fn state_dir() -> PathBuf {
    phux_config::instance::state_dir()
}

/// Install the subscriber for a server or one-shot CLI process, before the
/// runtime is built.
///
/// Logs to stderr, and also to `PHUX_LOG` when set (hold the returned
/// [`WorkerGuard`] for the process). Installs no panic hook: daemons call
/// [`install_server_panic_hook`] themselves, so a CLI panic is never
/// reported as a server panic.
///
/// # Errors
///
/// A subscriber is already installed, or the log file cannot be opened.
pub fn init() -> Result<Option<WorkerGuard>, Box<dyn std::error::Error + Send + Sync>> {
    let format = LogFormat::from_env();

    // Always-on stderr layer: ANSI only for an interactive operator. An
    // auto-spawned or supervised server's stderr is the server log file.
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let stderr_layer = fmt_layer(format, std::io::stderr as fn() -> std::io::Stderr, ansi);

    // Optional file tee, without ANSI codes.
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

    // `console_subscriber::spawn()` panics without `--cfg tokio_unstable`,
    // so gate on the cfg as well as the feature.
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

/// Install the subscriber for a client/TUI process, before raw mode.
///
/// File only (`PHUX_LOG` or [`default_client_log_path`]); honors the same
/// format and filter as [`init`]. The client's own panic hook logs before
/// restoring the terminal.
///
/// # Errors
///
/// A subscriber is already installed, or the log file cannot be opened.
pub fn init_client() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let format = LogFormat::from_env();
    let path = std::env::var_os(ENV_LOG_PATH)
        .filter(|v| !v.is_empty())
        .map_or_else(default_client_log_path, PathBuf::from);

    // Synchronous writer: `std::process::exit` skips guard flushes.
    let file_layer = fmt_layer(format, appender(&path)?, false);

    tracing_subscriber::registry()
        .with(env_filter())
        .with(file_layer)
        .try_init()?;

    Ok(())
}

/// Whether the server panic hook is installed (re-install would chain).
static SERVER_PANIC_HOOK_INSTALLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Install a panic hook that logs the message and backtrace through `tracing`
/// (and synchronously to `PHUX_LOG`), then chains the previous hook.
///
/// Idempotent. For long-running daemons only, after [`init`]. The backtrace
/// honors `RUST_BACKTRACE`.
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
        // Under `panic = "abort"` the queued event is never flushed; write it
        // synchronously too.
        append_panic_record_synchronously(&location, &info.to_string(), &backtrace);
        previous(info);
    }));
}

/// Append a panic record straight to `PHUX_LOG`, bypassing the non-blocking
/// appender whose guard never flushes under `panic = "abort"`. A duplicate
/// under unwind beats silence. Every failure is swallowed: a panic here
/// would abort with no record.
fn append_panic_record_synchronously(
    location: &str,
    message: &str,
    backtrace: &std::backtrace::Backtrace,
) {
    let Some(path) = std::env::var_os(ENV_LOG_PATH).filter(|value| !value.is_empty()) else {
        return;
    };
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // 0o600, like `harden_log_sink` (ADR-0028).
        options.mode(0o600);
    }
    if let Ok(mut file) = options.open(PathBuf::from(path)) {
        use std::io::Write as _;
        let _ = writeln!(
            file,
            "server panic (synchronous crash record) location={location} message={message} backtrace={backtrace}"
        );
        let _ = file.flush();
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    /// Run `f` with `key` set (or removed), then restore it. Process-global
    /// env: nextest isolates tests.
    fn with_env<R>(key: &str, value: Option<&std::ffi::OsStr>, f: impl FnOnce() -> R) -> R {
        let prev = std::env::var_os(key);
        let set = |value: Option<&std::ffi::OsStr>| match value {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        };
        set(value);
        let out = f();
        set(prev.as_deref());
        out
    }

    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).expect("read")
    }

    #[test]
    fn log_format_from_env_parses_json_case_insensitively() {
        for (value, format) in [
            (Some("JSON"), LogFormat::Json),
            (Some("text"), LogFormat::Text),
            (None, LogFormat::Text),
        ] {
            let value = value.map(std::ffi::OsStr::new);
            assert_eq!(with_env(ENV_LOG_FORMAT, value, LogFormat::from_env), format);
        }
    }

    #[test]
    fn log_paths_honor_xdg_state_home_and_scope_the_client_by_pid() {
        let leaf = phux_config::instance::state_dir()
            .file_name()
            .expect("the state dir always has a final component")
            .to_string_lossy()
            .into_owned();
        let custom = with_env(
            "XDG_STATE_HOME",
            Some("/custom/state".as_ref()),
            server_log_path,
        );
        assert_eq!(
            custom,
            PathBuf::from(format!("/custom/state/{leaf}/server.log"))
        );
        // Empty behaves as unset.
        let fallback = with_env("XDG_STATE_HOME", Some("".as_ref()), server_log_path);
        assert!(
            fallback.ends_with(format!(".local/state/{leaf}/server.log")),
            "{fallback:?}"
        );

        let client = default_client_log_path();
        let name = format!("client-{}.log", std::process::id());
        assert_eq!(client, state_dir().join(name));
    }

    /// The synchronous crash record lands without a guard, at 0o600, and
    /// only when `PHUX_LOG` names a sink.
    #[test]
    fn panic_record_is_written_synchronously_at_owner_only_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("panic-sync.log");
        let record = || {
            append_panic_record_synchronously(
                "src/thing.rs:42:7",
                "panicked at 'kaboom'",
                &std::backtrace::Backtrace::disabled(),
            );
        };
        with_env(ENV_LOG_PATH, None, record);
        assert!(!path.exists());
        with_env(ENV_LOG_PATH, Some(path.as_os_str()), record);
        let contents = read(&path);
        assert!(contents.contains("src/thing.rs:42:7") && contents.contains("kaboom"));
        assert_eq!(mode(&path), 0o600);
    }

    /// The writer creates parent and file at 0o600 (re-tightening a looser
    /// one) and flushes on guard drop.
    #[test]
    fn file_writer_creates_an_owner_only_sink_and_flushes_on_drop() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        use tracing_subscriber::fmt::MakeWriter as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("phux-test.log");
        {
            let (writer, _guard) = file_writer(&path).expect("file writer");
            assert_eq!(mode(&path), 0o600);
            writeln!(writer.make_writer(), "{{\"hello\":\"world\"}}").expect("write");
        }
        let line = read(&path);
        let parsed: serde_json::Value = serde_json::from_str(line.trim()).expect("JSON line");
        assert_eq!(parsed["hello"], "world");

        let loose = dir.path().join("loose.log");
        std::fs::write(&loose, b"old line\n").expect("seed file");
        std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let _writer = file_writer(&loose).expect("file writer");
        assert_eq!(mode(&loose), 0o600);
    }

    #[test]
    fn shift_plan_orders_highest_generation_first_within_the_cap() {
        let base = Path::new("/state/phux/server.log");
        let shift = |from, to| (generation_path(base, from), generation_path(base, to));
        assert_eq!(
            shift_plan(base, 4),
            vec![shift(3, 4), shift(2, 3), shift(1, 2)]
        );
        assert!(shift_plan(base, 1).is_empty());
        assert!(shift_plan(base, 0).is_empty());
    }

    /// Below the threshold nothing moves; above it `.1` gets the content at
    /// 0o600 and the live file is truncated in place (keeping its mode), so a `tail -f` reader
    /// keeps its inode.
    #[test]
    fn rotate_log_copies_to_generation_one_and_truncates_in_place() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.log");
        std::fs::write(&path, b"pre-rotation\n").expect("seed");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        assert!(!rotate_log(&path, 1024, 4).expect("rotate check"));
        assert!(!generation_path(&path, 1).exists());
        assert_eq!(read(&path), "pre-rotation\n");

        let reader = std::fs::File::open(&path).expect("open before rotation");
        let inode = reader.metadata().expect("metadata").ino();
        assert!(rotate_log(&path, 4, 4).expect("rotate"));
        assert_eq!(std::fs::metadata(&path).expect("metadata").ino(), inode);
        assert_eq!(read(&path), "");
        assert_eq!(read(&generation_path(&path, 1)), "pre-rotation\n");
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&generation_path(&path, 1)), 0o600);
    }

    #[test]
    fn rotate_log_caps_retained_generations_dropping_the_oldest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.log");
        let (gen1, gen2) = (generation_path(&path, 1), generation_path(&path, 2));
        std::fs::write(&path, b"live\n").expect("seed live");
        std::fs::write(&gen1, b"one\n").expect("seed .1");
        std::fs::write(&gen2, b"two\n").expect("seed .2");

        assert!(rotate_log(&path, 1, 2).expect("rotate"));
        assert_eq!(
            (read(&gen1), read(&gen2)),
            ("live\n".into(), "one\n".into())
        );
        assert!(!generation_path(&path, 3).exists());
    }

    /// The periodic check rotates the canonical path with production limits.
    #[test]
    fn rotate_server_log_if_needed_rotates_the_canonical_path_when_oversized() {
        let dir = tempfile::tempdir().expect("tempdir");
        with_env("XDG_STATE_HOME", Some(dir.path().as_os_str()), || {
            let path = server_log_path();
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            let oversized = usize::try_from(LOG_ROTATE_THRESHOLD_BYTES + 1).expect("fits");
            std::fs::write(&path, vec![b'x'; oversized]).expect("seed");
            assert!(rotate_server_log_if_needed().expect("rotation check"));
            assert!(generation_path(&path, 1).exists());
            assert_eq!(read(&path), "");
        });
    }
}
