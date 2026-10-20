//! `phux server`: configure, start, and supervise the foreground server.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_config::loader as config_loader;
use phux_server::runtime::default_socket_path;
use phux_server::{ServerConfig, ServerRuntime};

use crate::print_banner;

/// The fatal message for a config that exists but fails to load, shared by
/// stderr and the server log.
pub(super) fn broken_config_message(path: &Path, err: &impl std::fmt::Display) -> String {
    format!(
        "server: cannot start: config at {} failed to load\n  {err}\nrun `phux config check`",
        path.display()
    )
}

/// Log the startup line (pid, version, socket) that attributes what follows
/// in the shared server log, and record the start for crash-loop detection.
pub(super) fn log_startup(socket_path: &Path) {
    let pid = std::process::id();
    tracing::info!(
        pid,
        version = %env!("CARGO_PKG_VERSION"),
        socket = %socket_path.display(),
        "phux server started",
    );
    phux_server::health::record_start(pid, env!("CARGO_PKG_VERSION"));
}

pub(super) fn select_connectors(
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
    // `PHUX_DEV=1` turns on heap accounting from the first allocation.
    if phux_perf::dev::init_from_env() {
        tracing::info!(
            alloc_installed = phux_perf::alloc::installed(),
            "dev mode on ({}): heap accounting enabled",
            phux_perf::dev::ENV
        );
    }

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
        crate::output::error_block(msg.lines());
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
pub(super) fn build_server_config(
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
    let login_shell = std::env::var_os(crate::commands::service::SERVICE_MANAGED_ENV).is_some();

    // `--seed-command` runs via `<shell> -c` as the seeded session's program.
    let seed_command = seed_command
        .map(|command| phux_server::terminal_actor::shell_command(&shell, command, login_shell));

    ServerConfig {
        socket_path: socket_path.to_path_buf(),
        native_host_executable: None,
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
        // The durable organization store under the instance's state
        // directory (ADR-0162).
        org_store: phux_server::OrgStoreLocation::for_socket(socket_path),
    }
}

/// Build the current-thread tokio runtime, or report why it could not be
/// built.
fn build_runtime() -> Result<tokio::runtime::Runtime, ExitCode> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            error!("failed to build runtime: {err}");
            ExitCode::FAILURE
        })
}

/// The listener/feature suffix of the "phux server listening on ..." line:
/// every remote listener startup bound (or failed to), at its bound address,
/// and every mode this server was asked for.
fn listener_summary(
    listeners: &phux_protocol::wire::RemoteListenersReport,
    hub: bool,
    connector_count: usize,
    exit_after_idle: Option<u64>,
) -> String {
    use phux_protocol::wire::RemoteListenerTransport;
    use std::fmt::Write as _;
    let mut extra = String::new();
    for slot in &listeners.listeners {
        let scheme = match slot.transport {
            RemoteListenerTransport::Wss => "ws://",
            RemoteListenerTransport::Quic => "quic://",
            // WebTransport session URLs are https:// (HTTP/3 CONNECT).
            RemoteListenerTransport::Wt => "webtransport https://",
            _ => "",
        };
        let addr = slot.addr.as_deref().unwrap_or("?");
        let _ = write!(extra, " + {scheme}{addr}");
        if !slot.bound {
            let reason = slot.disabled_reason.map_or(
                "unknown",
                phux_protocol::wire::ListenerDisabledReason::as_str,
            );
            let _ = write!(extra, " (disabled: {reason})");
        }
    }
    if hub {
        extra.push_str(" [hub]");
    }
    if connector_count > 0 {
        let _ = write!(extra, " + connectors={connector_count}");
    }
    if let Some(secs) = exit_after_idle {
        let _ = write!(extra, " [exit-after-idle={secs}s]");
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
            info!("server: shutting down cleanly");
            ExitCode::SUCCESS
        }
        Err(err) => {
            error!("server failed: {err}");
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
            Box::new(crate::commands::archive::AutosaveArchiver::default()),
        )
    }) {
        None => None,
        Some(Ok(autosave)) => Some(autosave),
        Some(Err(err)) => {
            error!("server: cannot start: --autosave: {err}");
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
    let uplinks = config.uplink;
    if let Err(err) = phux_server::uplink::plan_uplinks(&uplinks) {
        error!("server failed: {err}");
        return ExitCode::FAILURE;
    }

    let configured_connectors = config.connector;
    let connector_entries = select_connectors(configured_connectors, connect.as_deref());
    if let Err(err) = phux_server::connector::plan_connectors(&connector_entries) {
        error!("server failed: connector: {err}");
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

    log_startup(&socket_path);

    // Printed once the sockets are bound, so a failed start never claims
    // to be listening.
    let banner_session = session.unwrap_or("none").to_owned();
    let connector_count = connector_entries.len();
    let mut server = with_network_listeners(ServerRuntime::new(cfg), listen, quic, webtransport)
        .on_listening(move |listeners| {
            let extra = listener_summary(listeners, hub, connector_count, exit_after_idle);
            info!(
                "server listening on {}{extra} (session={banner_session}; Ctrl-C to stop)",
                socket_path.display(),
            );
        });
    if !connector_entries.is_empty() {
        server = server.connectors(connector_entries, connect);
    }
    if let Some(autosave) = autosave {
        server = server.autosave(autosave);
    }
    // The config-reload doorbell re-reads `[[satellites]]`. A server that
    // booted with `--hub` redials the diff. One that did not still grows
    // the links when enrollment adds the first satellite (phux-lpn7.3),
    // without restarting panes.
    server = server.hub_reload(phux_server::SatelliteSource::new(|| {
        config_loader::load()
            .map(|config| config.satellites)
            .map_err(|err| err.to_string())
    }));
    // Satellite uplinks (ADR-0182): `phux host join` writes `[[uplink]]` and
    // rings the doorbell, which starts the new uplink without a restart.
    server = server
        .uplinks(uplinks)
        .uplink_reload(phux_server::uplink::UplinkSource::new(|| {
            config_loader::load()
                .map(|config| config.uplink)
                .map_err(|err| err.to_string())
        }));
    // Hub mode (ADR-0007): the runtime validates the satellite registry
    // before it accepts clients.
    if hub {
        server = server.hub(satellites);
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
        warn!(
            "server: could not install a SIGTERM handler; only Ctrl-C will stop this server cleanly"
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
pub(super) fn open_server_log(path: &Path) -> std::io::Result<std::fs::File> {
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
