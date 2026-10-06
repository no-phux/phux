use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::{Duration, Instant};

use phux_config::loader as config_loader;
use phux_config::socket::{self, SocketState};
use phux_server::runtime::default_socket_path;
use phux_server::{ServerConfig, ServerRuntime};

use crate::print_banner;

pub(super) mod ensure;

pub use ensure::ENSURE_TIMEOUT_ENV;

/// How long the auto-spawn path waits for a fresh server to accept (bind is
/// sub-ms on a healthy system; 2s tolerates a slow host).
const AUTO_SPAWN_SOCKET_TIMEOUT: Duration = Duration::from_secs(2);

/// Poll cadence while waiting for the auto-spawned server's socket.
const AUTO_SPAWN_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long a client waits for the spawn lock before spawning unserialised:
/// long enough to observe the holder's spawn, short enough not to hang.
const SPAWN_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Version of the `phux server --ensure --json` availability document.
const ENSURE_SCHEMA_VERSION: u8 = 1;

/// Which owner made the selected socket available. `--ensure` proves only
/// that the socket accepts; HELLO still negotiates protocol and features.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnsureDisposition {
    /// The socket accepted before startup coordination was needed.
    Reused,
    /// Another caller made it available while this caller waited for the lock.
    Joined,
    /// A pending `service install --adopt` handover started the supervisor.
    SupervisedStarted,
    /// This process launched the detached server directly.
    DaemonStarted,
}

impl EnsureDisposition {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Reused => "reused",
            Self::Joined => "joined",
            Self::SupervisedStarted => "supervised_started",
            Self::DaemonStarted => "daemon_started",
        }
    }
}

fn ensure_document(socket_path: &Path, disposition: EnsureDisposition) -> serde_json::Value {
    serde_json::json!({
        "schema_version": ENSURE_SCHEMA_VERSION,
        "running": true,
        "socket": socket_path.display().to_string(),
        "disposition": disposition.as_str(),
        "cli_version": env!("CARGO_PKG_VERSION"),
        "server_log": phux_server::telemetry::server_log_path(),
    })
}

fn report_ensure_failure(json: bool, socket_path: &Path, err: &std::io::Error) -> ExitCode {
    if !json {
        eprintln!("phux server --ensure: {}: {err}", socket_path.display());
        return ExitCode::FAILURE;
    }

    let code = match err.kind() {
        std::io::ErrorKind::TimedOut => super::json_err::codes::SERVER_START_TIMEOUT,
        std::io::ErrorKind::Interrupted => super::json_err::codes::SERVER_START_CANCELLED,
        _ => super::json_err::codes::SERVER_START_FAILED,
    };
    let log_path = phux_server::telemetry::server_log_path();
    let error = super::json_err::CliError::new(
        code,
        format!(
            "could not make the local server available at {}: {err}",
            socket_path.display()
        ),
        format!(
            "retry; inspect {}; run `phux config check` and `phux doctor`",
            log_path.display()
        ),
    );
    super::json_err::emit(true, &error, 1)
}

/// Noninteractive counterpart of naked `phux`'s coordinator startup.
pub(crate) fn run_ensure(socket: Option<PathBuf>, json: bool) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    if let Err(err) = phux_server::runtime::validate_socket_path_len(&socket_path) {
        return report_ensure_failure(
            json,
            &socket_path,
            &std::io::Error::new(std::io::ErrorKind::InvalidInput, err),
        );
    }
    match ensure::with_deadline(socket_path.clone()) {
        Ok(disposition) => {
            if json {
                outln!("{}", ensure_document(&socket_path, disposition));
            }
            ExitCode::SUCCESS
        }
        Err(err) => report_ensure_failure(json, &socket_path, &err),
    }
}

fn ensure_accepting(socket_path: &Path) -> std::io::Result<EnsureDisposition> {
    let disposition = ensure_server(
        socket_path,
        &super::attach::resolved_default_session_name(),
        super::attach::configured_spawn_on_attach().as_deref(),
        // Availability-only: the quiet path skips version reconciliation, so a
        // bundled CLI does not re-exec another installation's coordinator.
        true,
    )?;
    // The probe calls permission errors "Live" (to avoid unlinking another
    // user's socket); success here requires a real connect.
    std::os::unix::net::UnixStream::connect(socket_path).map(|stream| {
        drop(stream);
        disposition
    })
}

/// The fatal message for a config that exists but fails to load, shared by
/// stderr and the server log.
fn broken_config_message(path: &Path, err: &impl std::fmt::Display) -> String {
    format!(
        "phux server: cannot start: config at {} failed to load\n  {err}\nrun: phux config check",
        path.display()
    )
}

/// Log the startup line (pid, version, socket) that attributes what follows
/// in the shared server log, and record the start for crash-loop detection.
fn log_startup(socket_path: &Path) {
    let pid = std::process::id();
    tracing::info!(
        pid,
        version = %env!("CARGO_PKG_VERSION"),
        socket = %socket_path.display(),
        "phux server started",
    );
    phux_server::health::record_start(pid, env!("CARGO_PKG_VERSION"));
}

fn select_connectors(
    configured: Vec<phux_config::ConnectorConfigEntry>,
    connect: Option<&str>,
) -> Vec<phux_config::ConnectorConfigEntry> {
    match connect {
        Some(relay) => vec![
            configured
                .into_iter()
                .find(|entry| entry.relay == relay)
                .unwrap_or_else(|| phux_config::ConnectorConfigEntry {
                    relay: relay.to_owned(),
                    token_file: None,
                    cert_fingerprint: None,
                }),
        ],
        None => configured,
    }
}

/// Arm the process-wide server concerns and resolve the socket path, or
/// report why the server refuses to start, before any config or runtime work.
fn prepare_process(
    socket: Option<PathBuf>,
    daemonize: bool,
    resume: Option<std::os::fd::RawFd>,
) -> Result<PathBuf, ExitCode> {
    // Durable crash capture for this long-running, often detached process.
    phux_server::telemetry::install_server_panic_hook();

    let socket_path = socket.unwrap_or_else(default_socket_path);
    // Fail before the banner when the path cannot fit in a `sockaddr_un`.
    crate::commands::ensure_socket_path_fits(&socket_path)?;

    // Banner only for a hand-started foreground server.
    if !daemonize && resume.is_none() {
        print_banner();
    }

    // Auto-spawn: detach from the launching terminal so closing it cannot
    // SIGHUP the server. `EPERM` (already a group leader) is harmless.
    if daemonize {
        let _ = rustix::process::setsid();
    }

    Ok(socket_path)
}

/// Load the one config snapshot every consumer binds from, so an edit
/// mid-startup cannot yield a torn read. A missing file yields defaults; a file
/// that fails to load is fatal, because silently dropping hooks and policy
/// behind a normal banner is worse than refusing to start.
fn load_config() -> Result<phux_config::Config, ExitCode> {
    config_loader::load().map_err(|err| {
        let msg = broken_config_message(&config_loader::config_path(), &err);
        eprintln!("{msg}");
        tracing::error!(
            path = %config_loader::config_path().display(),
            error = %err,
            "refusing to start: config failed to load; run: phux config check"
        );
        ExitCode::FAILURE
    })
}

/// Compose the `ServerConfig` the runtime binds from, out of the single
/// config snapshot's `defaults` and the flags that override them.
#[allow(
    clippy::too_many_arguments,
    reason = "one config snapshot's independent sections, threaded through the single startup call"
)]
fn build_server_config(
    session: Option<&str>,
    socket_path: &Path,
    defaults: phux_config::DefaultsCfg,
    voice: phux_config::VoiceCfg,
    limits: &phux_config::LimitsCfg,
    hook_catalog: phux_server::hooks::HookCatalog,
    seed_command: Option<&str>,
    exit_after_idle: Option<u64>,
) -> ServerConfig {
    // Resolve the default shell once (`defaults.shell`, `$SHELL`, `/bin/sh`)
    // for every server-owned spawn path.
    let shell = phux_server::terminal_actor::resolve_shell(defaults.shell.as_deref());

    // A server started from a unit `phux service install` wrote (marked by
    // `SERVICE_MANAGED_ENV`) never ran a login shell, so its panes need login-shell
    // treatment to see profile `PATH` entries. Absent the marker, a login shell
    // would re-run initialization that is not idempotent for every setup.
    let login_shell = std::env::var_os(super::service::SERVICE_MANAGED_ENV).is_some();

    // `--seed-command` runs via `<shell> -c` as the seeded session's program.
    let seed_command = seed_command
        .map(|command| phux_server::terminal_actor::shell_command(&shell, command, login_shell));

    ServerConfig {
        socket_path: socket_path.to_path_buf(),
        // `None` under `--no-seed` (ADR-0105).
        pre_seeded_session: session.map(str::to_owned),
        seed_with_pty: true,
        seed_command,
        scrollback: defaults.scrollback_limits(),
        agent_log_bytes: defaults.agent_log_bytes,
        event_journal_entries: defaults.event_journal_entries,
        event_journal_bytes: defaults.event_journal_bytes,
        retain: phux_server::state::RetainPolicy {
            by_default: defaults.retain_on_exit,
            default_secs: defaults.retain_on_exit_secs,
            max_secs: defaults.retain_on_exit_max_secs,
            max_count: defaults
                .retain_on_exit_max
                .min(phux_config::MAX_RETAIN_ON_EXIT_MAX),
        },
        cwd_inheritance: defaults.cwd_inheritance,
        term: defaults.term,
        shell,
        login_shell,
        window_size: defaults.window_size,
        voice,
        // Clamped up so the built-in agent-session record write always fits under
        // the generic metadata cap, whatever the config says.
        metadata_value_bytes: limits.metadata_value_bytes.max(
            u32::try_from(phux_protocol::wire::frame::MAX_AGENT_SESSION_RECORD_BYTES)
                .unwrap_or(u32::MAX),
        ),
        approval_ttl_secs: defaults
            .approval_ttl_secs
            .min(phux_config::MAX_APPROVAL_TTL_SECS),
        approval_max_pending: defaults
            .approval_max_pending
            .min(phux_config::MAX_APPROVAL_MAX_PENDING),
        approval_max_pending_total: defaults
            .approval_max_pending_total
            .min(phux_config::MAX_APPROVAL_MAX_PENDING_TOTAL),
        policy_engine: None,
        policy_mode: None,
        hook_catalog,
        // Ephemeral lifetime (ADR-0063); absent by default.
        exit_after_idle: exit_after_idle.map(Duration::from_secs),
        // The one place the `PHUX_*` process configuration is read.
        env: phux_server::ServerEnv::from_process(),
    }
}

/// Build the current-thread tokio runtime, or report why it could not be
/// built.
fn build_runtime() -> Result<tokio::runtime::Runtime, ExitCode> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            eprintln!("failed to build runtime: {err}");
            ExitCode::FAILURE
        })
}

/// The listener/feature suffix of the "phux server listening on ..." line:
/// every extra endpoint and mode this server was asked for.
fn listener_summary(
    listen: Option<std::net::SocketAddr>,
    quic: Option<std::net::SocketAddr>,
    webtransport: Option<std::net::SocketAddr>,
    hub: bool,
    connector_count: usize,
    exit_after_idle: Option<u64>,
) -> String {
    let mut extra = match (listen, quic) {
        (Some(ws), Some(q)) => format!(" + ws://{ws} + quic://{q}"),
        (Some(ws), None) => format!(" + ws://{ws}"),
        (None, Some(q)) => format!(" + quic://{q}"),
        (None, None) => String::new(),
    };
    if let Some(wt) = webtransport {
        // WebTransport session URLs are https:// (HTTP/3 CONNECT).
        let _ =
            std::fmt::Write::write_fmt(&mut extra, format_args!(" + webtransport https://{wt}"));
    }
    if hub {
        extra.push_str(" [hub]");
    }
    if connector_count > 0 {
        let _ =
            std::fmt::Write::write_fmt(&mut extra, format_args!(" + connectors={connector_count}"));
    }
    if let Some(secs) = exit_after_idle {
        let _ = std::fmt::Write::write_fmt(&mut extra, format_args!(" [exit-after-idle={secs}s]"));
    }
    extra
}

/// Attach the optional network listeners the flags asked for.
#[allow(
    clippy::missing_const_for_fn,
    reason = "the WebTransport-disabled listener logs a warning and is not const"
)]
fn with_network_listeners(
    mut server: ServerRuntime,
    listen: Option<std::net::SocketAddr>,
    quic: Option<std::net::SocketAddr>,
    webtransport: Option<std::net::SocketAddr>,
) -> ServerRuntime {
    if let Some(addr) = listen {
        server = server.listen_ws(addr);
    }
    if let Some(addr) = quic {
        server = server.listen_quic(addr);
    }
    if let Some(addr) = webtransport {
        server = server.listen_webtransport(addr);
    }
    server
}

/// Report how the runtime stopped and map it to the process exit code.
fn report_shutdown<E: std::fmt::Display>(result: Result<(), E>) -> ExitCode {
    match result {
        Ok(()) => {
            eprintln!("phux server: shutting down cleanly");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("phux server failed: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Build a current-thread tokio runtime and drive `ServerRuntime` until
/// SIGINT or SIGTERM.
#[allow(
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools,
    reason = "1:1 mirror of the `phux server` clap surface; bundling into a struct would just restate the clap enum"
)]
pub(crate) fn run_server(
    session: Option<&str>,
    socket: Option<PathBuf>,
    listen: Option<std::net::SocketAddr>,
    quic: Option<std::net::SocketAddr>,
    webtransport: Option<std::net::SocketAddr>,
    connect: Option<String>,
    hub: bool,
    exit_after_idle: Option<u64>,
    autosave: Option<&Path>,
    daemonize: bool,
    seed_command: Option<&str>,
    resume: Option<std::os::fd::RawFd>,
) -> ExitCode {
    let socket_path = match prepare_process(socket, daemonize, resume) {
        Ok(path) => path,
        Err(code) => return code,
    };

    // Refused before anything binds: a dev build never writes production state.
    let autosave = match autosave.map(|path| {
        phux_server::autosave::Autosave::new(
            path,
            Box::new(super::workspace::AutosaveArchiver::default()),
        )
    }) {
        None => None,
        Some(Ok(autosave)) => Some(autosave),
        Some(Err(err)) => {
            eprintln!("phux server: cannot start: --autosave: {err}");
            return ExitCode::FAILURE;
        }
    };

    let config = match load_config() {
        Ok(config) => config,
        Err(code) => return code,
    };

    // Config hooks plus enabled plugin manifests' events feed the server-side
    // hook dispatcher.
    let hook_catalog =
        phux_server::hooks::HookCatalog::from_config(&config, &config_loader::config_path());

    let satellites = config.satellites;

    let configured_connectors = config.connector;
    let connector_entries = select_connectors(configured_connectors, connect.as_deref());
    if let Err(err) = phux_server::connector::plan_connectors(&connector_entries) {
        eprintln!("phux server failed: connector: {err}");
        return ExitCode::FAILURE;
    }

    let mut cfg = build_server_config(
        session,
        &socket_path,
        config.defaults,
        config.voice,
        &config.limits,
        hook_catalog,
        seed_command,
        exit_after_idle,
    );
    cfg.policy_mode = config.policy.mode;

    let rt = match build_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    let extra = listener_summary(
        listen,
        quic,
        webtransport,
        hub,
        connector_entries.len(),
        exit_after_idle,
    );
    eprintln!(
        "phux server listening on {}{extra} (session={}; Ctrl-C to stop)",
        socket_path.display(),
        session.unwrap_or("none")
    );
    log_startup(&socket_path);

    let mut server = with_network_listeners(ServerRuntime::new(cfg), listen, quic, webtransport);
    if !connector_entries.is_empty() {
        server = server.connectors(connector_entries, connect);
    }
    if let Some(autosave) = autosave {
        server = server.autosave(autosave);
    }
    // Hub mode (ADR-0007): the runtime validates the satellite registry.
    // The config-reload doorbell re-reads `[[satellites]]` the same way, so
    // `phux host add --role satellite` reaches a running hub (phux-lpn7.2).
    if hub {
        server = server
            .hub(satellites)
            .hub_reload(phux_server::SatelliteSource::new(|| {
                config_loader::load()
                    .map(|config| config.satellites)
                    .map_err(|err| err.to_string())
            }));
    }
    // Every start consumes (resume) or discards the upgrade handoff.
    server = match resume {
        Some(fd) => server.resume(fd),
        None => server.discard_inherited_upgrade(),
    };
    // Live rotation of the server log for as long as this process runs.
    rt.spawn(phux_server::telemetry::run_log_rotation_task());

    // Marks the whole server's keystroke path interactive (ADR-0096).
    phux_server::perf::mark_started();
    report_shutdown(rt.block_on(async move { server.run_async(shutdown_signal()).await }))
}

/// Resolve on SIGINT or SIGTERM. Both cancel the runtime's root token, the
/// same graceful path idle-exit uses: panes are hung up and reaped and the
/// socket is unlinked. Without SIGTERM a supervised stop died on the default
/// disposition, leaving a stale socket and an exit status launchd reads as a
/// crash (ADR-0080).
async fn shutdown_signal() {
    let interrupt = tokio::signal::ctrl_c();

    let Ok(mut terminate) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    else {
        // Registering failed; fall back to SIGINT alone rather than refuse to serve.
        eprintln!(
            "phux server: could not install a SIGTERM handler; only Ctrl-C will stop this server cleanly"
        );
        let _ = interrupt.await;
        return;
    };

    tokio::select! {
        _ = interrupt => {}
        _ = terminate.recv() => {}
    }
}

/// Open the server log for appending: parent dir `0o700`, file `0o600`
/// (ADR-0028).
fn open_server_log(path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder.create(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

/// Environment variable that gives an auto-spawned daemon an idle backstop,
/// in seconds (`1..=86_400`, as `--exit-after-idle`). Public so the
/// integration harness re-arms it by name.
pub const AUTO_SPAWN_IDLE_ENV: &str = "PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE";

/// The upper bound `--exit-after-idle` accepts.
const AUTO_SPAWN_IDLE_MAX_SECS: u64 = 86_400;

/// Resolve the idle limit an auto-spawned daemon should carry, if any.
/// `None` in production: an unattended server stays up (ADR-0063); the
/// variable is an opt-in for test harnesses so auto-spawned daemons cannot
/// leak. A malformed value warns (unless `quiet`) and is ignored.
fn auto_spawn_idle_limit(quiet: bool) -> Option<u64> {
    let raw = std::env::var_os(AUTO_SPAWN_IDLE_ENV)?;
    let text = raw.to_string_lossy();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parsed = parse_auto_spawn_idle(trimmed);
    if parsed.is_none() && !quiet {
        eprintln!(
            "phux: ignoring {AUTO_SPAWN_IDLE_ENV}={trimmed} \
             (want whole seconds, 1..={AUTO_SPAWN_IDLE_MAX_SECS}); \
             the auto-spawned server will have no idle limit"
        );
    }
    parsed
}

/// The pure half of [`auto_spawn_idle_limit`], matching the flag's range.
fn parse_auto_spawn_idle(raw: &str) -> Option<u64> {
    raw.parse::<u64>()
        .ok()
        .filter(|secs| (1..=AUTO_SPAWN_IDLE_MAX_SECS).contains(secs))
}

/// Fork-exec the current binary as a detached `phux server --daemonize`, with
/// stderr on the canonical server log, and wait for it to accept. The child is
/// not kept: it owns its own lifecycle.
pub(crate) fn maybe_auto_spawn_server(
    socket_path: &Path,
    session: Option<&str>,
    seed_command: Option<&str>,
    quiet: bool,
    login_shell: bool,
) -> std::io::Result<()> {
    let current_exe = std::env::current_exe()?;
    let log_path = phux_server::telemetry::server_log_path();

    // The banner names the log; suppressed under `--json`.
    if !quiet {
        eprintln!(
            "phux: starting server at {} (auto-spawn, session={}; log: {})",
            socket_path.display(),
            session.unwrap_or("none"),
            log_path.display()
        );
    }

    // Best-effort: fall back to /dev/null if the log cannot be opened.
    let log = open_server_log(&log_path).ok();
    // Where this daemon's output starts in the shared, append-only log.
    let log_offset = log
        .as_ref()
        .and_then(|file| file.metadata().ok())
        .map(|metadata| metadata.len());

    let mut cmd = std::process::Command::new(current_exe);
    cmd.arg("server")
        .arg("--socket")
        .arg(socket_path)
        .arg("--daemonize")
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    // A server started from a non-login environment gets the service marker, so
    // its panes run login shells (ADR-0120).
    if login_shell {
        cmd.env(super::service::SERVICE_MANAGED_ENV, "1");
    }
    match session {
        Some(name) => {
            cmd.arg("--session").arg(name);
        }
        None => {
            cmd.arg("--no-seed");
        }
    }
    if let Some(seed) = seed_command {
        cmd.arg("--seed-command").arg(seed);
    }
    // Passed as a flag so a leaked server shows its bound in `ps`.
    if let Some(secs) = auto_spawn_idle_limit(quiet) {
        cmd.arg("--exit-after-idle").arg(secs.to_string());
    }
    match log {
        Some(file) => {
            cmd.stderr(file);
        }
        None => {
            cmd.stderr(Stdio::null());
        }
    }

    // `--daemonize` only calls `setsid`, so this is the server process itself.
    // It is dropped (not killed) once accepting: it owns its own lifecycle.
    let mut child = ensure::spawn_daemon(&mut cmd)?;

    wait_until_accepting(socket_path, "auto-spawned server", &log_path, || {
        // `try_wait` is a non-blocking `waitpid`: watching the daemon this way
        // cannot change its lifetime, unlike another connect.
        let status = child.try_wait().ok().flatten()?;
        Some(early_exit_error(socket_path, status, &log_path, log_offset))
    })
}

/// The error for a spawned server that exited before accepting: its exit
/// status and the tail of what it wrote to the log (its bind or config error).
fn early_exit_error(
    socket_path: &Path,
    status: std::process::ExitStatus,
    log_path: &Path,
    log_offset: Option<u64>,
) -> std::io::Error {
    let output = log_offset
        .and_then(|offset| startup_output(log_path, offset))
        .map(|tail| format!(": {tail}"))
        .unwrap_or_default();
    std::io::Error::other(format!(
        "auto-spawned server exited before accepting on {} ({status}){output} (see {})",
        socket_path.display(),
        log_path.display(),
    ))
}

/// The last few lines a spawned server appended to its log from `offset`.
fn startup_output(log_path: &Path, offset: u64) -> Option<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    /// Enough for the fatal line and its context, not a whole crash dump.
    const MAX_LINES: usize = 3;
    const MAX_BYTES: u64 = 16 * 1024;
    let mut file = std::fs::File::open(log_path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(offset.max(len.saturating_sub(MAX_BYTES))))
        .ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let tail = lines[lines.len().saturating_sub(MAX_LINES)..].join("; ");
    (!tail.is_empty()).then_some(tail)
}

/// Block until `socket_path` accepts (not merely exists), or the auto-spawn
/// deadline passes. `what` names what is being waited on. `exited` reports a
/// server that died before accepting, which fails the wait at once instead of
/// at the deadline.
fn wait_until_accepting(
    socket_path: &Path,
    what: &str,
    log_path: &Path,
    mut exited: impl FnMut() -> Option<std::io::Error>,
) -> std::io::Result<()> {
    let deadline = Instant::now() + AUTO_SPAWN_SOCKET_TIMEOUT;
    loop {
        if socket::probe(socket_path) == SocketState::Live {
            return Ok(());
        }
        if let Some(err) = exited() {
            // A loser of a bind race exits because another server is live.
            if socket::probe(socket_path) == SocketState::Live {
                return Ok(());
            }
            return Err(err);
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "{what} did not accept on {} within {:?} (see {})",
                    socket_path.display(),
                    AUTO_SPAWN_SOCKET_TIMEOUT,
                    log_path.display(),
                ),
            ));
        }
        std::thread::sleep(AUTO_SPAWN_POLL_INTERVAL);
    }
}

/// Ensure a server is accepting on `socket_path`, starting one if not. The
/// single entry point every client verb uses: a stale socket is reaped rather
/// than trusted, and a profile-scoped lock elects one spawner among
/// concurrent invocations. A live server costs one probe and no lock.
pub(crate) fn ensure_server(
    socket_path: &Path,
    session: &str,
    seed_command: Option<&str>,
    quiet: bool,
) -> std::io::Result<EnsureDisposition> {
    ensure_server_with(socket_path, Some(session), seed_command, quiet, false)
}

/// [`ensure_server`] without a seed session (`phux new --empty`, ADR-0105).
pub(crate) fn ensure_server_unseeded(
    socket_path: &Path,
    quiet: bool,
) -> std::io::Result<EnsureDisposition> {
    ensure_server_with(socket_path, None, None, quiet, false)
}

/// [`ensure_server`] for `phux bootstrap` (`phux attach --ssh`, ADR-0120): a
/// server started here gets the login-shell marker, because ssh ran it
/// non-interactively.
pub(crate) fn ensure_server_for_bootstrap(socket_path: &Path) -> std::io::Result<()> {
    ensure_server_with(
        socket_path,
        Some(&super::attach::resolved_default_session_name()),
        super::attach::configured_spawn_on_attach().as_deref(),
        false,
        true,
    )
    .map(|_| ())
}

/// The shared body of the `ensure_server*` entry points.
fn ensure_server_with(
    socket_path: &Path,
    session: Option<&str>,
    seed_command: Option<&str>,
    quiet: bool,
    login_shell: bool,
) -> std::io::Result<EnsureDisposition> {
    if socket::probe(socket_path) == SocketState::Live {
        // A live socket may be a supervised server login started; retire a
        // leftover `--adopt` marker.
        super::service::sweep_stale_adoption_marker(socket_path);
        if !quiet {
            reconcile_version_skew(socket_path);
        }
        return Ok(EnsureDisposition::Reused);
    }

    // Serialise the spawn decision. Failing to lock is not fatal: the server's
    // own bind-time probe still rejects a duplicate.
    let guard = SpawnLock::acquire(&socket::spawn_lock_path(socket_path));

    // Re-probe under the lock: the previous holder likely spawned it.
    if socket::probe(socket_path) == SocketState::Live {
        super::service::sweep_stale_adoption_marker(socket_path);
        return Ok(EnsureDisposition::Joined);
    }

    // Nothing is accepting; remove a dead server's socket entry.
    if let Err(err) = socket::reap_stale(socket_path) {
        tracing::warn!(
            path = %socket_path.display(),
            error = %err,
            "could not remove stale socket entry",
        );
    }

    // A pending `--adopt` unit for this socket gets first refusal (ADR-0088);
    // an ordinary installed unit does not, so a stopped server stays stopped.
    if matches!(
        super::service::complete_pending_adoption(socket_path, quiet),
        super::service::Handover::Started
    ) {
        let result = wait_until_accepting(
            socket_path,
            "the supervised server",
            &phux_server::telemetry::server_log_path(),
            // The init system owns this process; there is no child to watch.
            || None,
        );
        drop(guard);
        return result.map(|()| EnsureDisposition::SupervisedStarted);
    }

    let result = maybe_auto_spawn_server(socket_path, session, seed_command, quiet, login_shell)
        .map(|()| EnsureDisposition::DaemonStarted);
    drop(guard);
    result
}

/// Hand a server running an older build over to this one in place
/// (ADR-0032 re-exec, panes survive), for binaries replaced by a package
/// manager. Best-effort: on refusal the old server keeps running.
fn reconcile_version_skew(socket_path: &Path) {
    let ours = env!("CARGO_PKG_VERSION");
    let Some(theirs) = phux_server::health::running_version() else {
        return;
    };
    if theirs == ours {
        return;
    }

    eprintln!(
        "phux: the running server is {theirs}, this binary is {ours} — upgrading it in place"
    );
    match super::upgrade::request_upgrade(socket_path) {
        Ok(super::upgrade::UpgradeAck::Upgrading) => {
            // Wait for the re-exec'd server so the caller's connect does not race it.
            let deadline = Instant::now() + AUTO_SPAWN_SOCKET_TIMEOUT;
            while Instant::now() < deadline {
                if socket::probe(socket_path) == SocketState::Live {
                    return;
                }
                std::thread::sleep(AUTO_SPAWN_POLL_INTERVAL);
            }
        }
        Ok(
            super::upgrade::UpgradeAck::Refused(message)
            | super::upgrade::UpgradeAck::Unexpected(message),
        ) => {
            eprintln!("phux: the server declined the upgrade ({message}); continuing on {theirs}");
        }
        Err(err) => {
            eprintln!("phux: could not upgrade the running server ({err}); continuing on {theirs}");
        }
    }
}

/// An advisory `flock` held for the duration of a spawn decision, scoped to
/// the profile's runtime dir. The lock file is never unlinked (a new inode
/// would not exclude holders of the old one).
struct SpawnLock(Option<std::fs::File>);

impl SpawnLock {
    /// Take the lock, waiting up to [`SPAWN_LOCK_TIMEOUT`]; unlocked on failure.
    fn acquire(path: &Path) -> Self {
        let Some(parent) = path.parent() else {
            return Self(None);
        };
        if std::fs::create_dir_all(parent).is_err() {
            return Self(None);
        }
        let Ok(file) = rustix::fs::open(
            path,
            rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        ) else {
            return Self(None);
        };
        let file = std::fs::File::from(file);
        if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
            return Self(None);
        }
        let deadline = Instant::now() + SPAWN_LOCK_TIMEOUT;
        loop {
            match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Self(Some(file)),
                Err(rustix::io::Errno::WOULDBLOCK) if Instant::now() < deadline => {
                    std::thread::sleep(AUTO_SPAWN_POLL_INTERVAL);
                }
                Err(_) => return Self(None),
            }
        }
    }
}

impl Drop for SpawnLock {
    fn drop(&mut self) {
        if let Some(file) = self.0.take() {
            // Best-effort: closing the descriptor releases the lock anyway.
            let _ = rustix::fs::flock(&file, rustix::fs::FlockOperation::Unlock);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_document_is_an_availability_result_not_a_handshake_claim() {
        let socket = Path::new("/tmp/phux-contract.sock");
        let doc = ensure_document(socket, EnsureDisposition::SupervisedStarted);
        assert_eq!(doc["schema_version"], u64::from(ENSURE_SCHEMA_VERSION));
        assert_eq!(doc["running"], true);
        assert_eq!(doc["socket"], socket.display().to_string());
        assert_eq!(doc["disposition"], "supervised_started");
        assert_eq!(doc["cli_version"], env!("CARGO_PKG_VERSION"));
        assert!(doc["server_log"].is_string());
        assert!(doc.get("protocol").is_none());
        assert!(doc.get("server_version").is_none());
    }

    #[test]
    fn spawn_lock_refuses_symlinks_and_fifos() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target");
        std::fs::write(&target, b"do not lock").expect("target");
        let symlink = dir.path().join("symlink.lock");
        std::os::unix::fs::symlink(&target, &symlink).expect("symlink");
        assert!(SpawnLock::acquire(&symlink).0.is_none());

        let fifo = dir.path().join("fifo.lock");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("run mkfifo");
        assert!(status.success(), "mkfifo must create the hostile fixture");
        assert!(SpawnLock::acquire(&fifo).0.is_none());
    }

    fn connector(relay: &str, token: &str) -> phux_config::ConnectorConfigEntry {
        phux_config::ConnectorConfigEntry {
            relay: relay.to_owned(),
            token_file: Some(PathBuf::from(token)),
            cert_fingerprint: Some("AB".to_owned()),
        }
    }

    /// The idle backstop accepts exactly what `--exit-after-idle` accepts.
    #[test]
    fn the_auto_spawn_idle_backstop_accepts_what_the_flag_accepts() {
        assert_eq!(parse_auto_spawn_idle("600"), Some(600));
        assert_eq!(parse_auto_spawn_idle("1"), Some(1));
        assert_eq!(parse_auto_spawn_idle("86400"), Some(86_400));

        assert_eq!(parse_auto_spawn_idle("0"), None);
        assert_eq!(parse_auto_spawn_idle("86401"), None);

        assert_eq!(parse_auto_spawn_idle(""), None);
        assert_eq!(parse_auto_spawn_idle("600s"), None);
        assert_eq!(parse_auto_spawn_idle("1.5"), None);
        assert_eq!(parse_auto_spawn_idle("-1"), None);
        assert_eq!(parse_auto_spawn_idle("forever"), None);
    }

    /// `metadata-value-bytes` is clamped up to the agent-session record floor and
    /// passes through above it.
    #[test]
    fn metadata_value_bytes_is_clamped_to_the_agent_session_record_floor() {
        let floor =
            u32::try_from(phux_protocol::wire::frame::MAX_AGENT_SESSION_RECORD_BYTES).unwrap();
        for (configured, expected) in [
            (0, floor),
            (
                phux_config::DEFAULT_METADATA_VALUE_BYTES,
                phux_config::DEFAULT_METADATA_VALUE_BYTES,
            ),
        ] {
            let cfg = build_server_config(
                None,
                Path::new("/tmp/phux-test.sock"),
                phux_config::DefaultsCfg::default(),
                phux_config::VoiceCfg::default(),
                &phux_config::LimitsCfg {
                    metadata_value_bytes: configured,
                },
                phux_server::hooks::HookCatalog::default(),
                None,
                None,
            );
            assert_eq!(cfg.metadata_value_bytes, expected);
        }
    }

    #[test]
    fn broken_config_message_names_path_error_and_remedy() {
        let msg = broken_config_message(
            Path::new("/home/u/.config/phux/config.toml"),
            &"config.toml: 3:14: expected `=` after key",
        );
        assert!(msg.contains("/home/u/.config/phux/config.toml"), "{msg}");
        assert!(
            msg.contains("config.toml: 3:14: expected `=` after key"),
            "{msg}"
        );
        assert!(msg.contains("run: phux config check"), "{msg}");
    }

    /// The startup line carries pid, version, and socket and passes the default
    /// filter into a file sink.
    #[test]
    #[allow(clippy::expect_used, reason = "test")]
    fn startup_line_lands_in_file_at_default_filter() {
        use tracing_subscriber::layer::SubscriberExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.log");
        let file = std::fs::File::create(&path).expect("create sink");
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false);
        let subscriber = tracing_subscriber::registry()
            // Mirrors telemetry::DEFAULT_FILTER.
            .with(tracing_subscriber::EnvFilter::new("phux=info,warn"))
            .with(layer);
        tracing::subscriber::with_default(subscriber, || {
            log_startup(Path::new("/run/user/1000/phux/phux.sock"));
        });

        let contents = std::fs::read_to_string(&path).expect("read back log");
        assert!(
            contents.contains("phux server started"),
            "startup line filtered out at the default filter: {contents}"
        );
        assert!(
            contents.contains(&format!("pid={}", std::process::id())),
            "pid missing: {contents}"
        );
        assert!(
            contents.contains(&format!("version={}", env!("CARGO_PKG_VERSION"))),
            "version missing: {contents}"
        );
        assert!(
            contents.contains("socket=/run/user/1000/phux/phux.sock"),
            "socket missing: {contents}"
        );
    }

    /// The server log opens under a `0o700` parent at `0o600` (ADR-0028).
    #[cfg(unix)]
    #[test]
    #[allow(clippy::expect_used, reason = "test")]
    fn open_server_log_creates_0700_parent_and_0600_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state").join("phux").join("server.log");
        let _file = open_server_log(&path).expect("open server log");

        let parent_mode = std::fs::metadata(path.parent().expect("parent"))
            .expect("parent metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(parent_mode, 0o700, "parent mode was {parent_mode:o}");
        let file_mode = std::fs::metadata(&path)
            .expect("file metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600, "log mode was {file_mode:o}");
    }

    #[test]
    fn connectors_all_run_by_default_and_connect_selects_one() {
        let configured = vec![
            connector("one.example:4433", "/one"),
            connector("two.example:4433", "/two"),
        ];
        assert_eq!(select_connectors(configured.clone(), None), configured);
        assert_eq!(
            select_connectors(configured, Some("two.example:4433")),
            vec![connector("two.example:4433", "/two")]
        );

        // An unconfigured relay is allowed ad hoc, without credentials.
        let selected = select_connectors(Vec::new(), Some("127.0.0.1:4433"));
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].relay, "127.0.0.1:4433");
        assert!(selected[0].token_file.is_none());
        assert!(selected[0].cert_fingerprint.is_none());
    }
}
