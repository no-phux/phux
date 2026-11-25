//! `phux server`: configure, start, and supervise the foreground server.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_config::loader as config_loader;
use phux_config::socket::default_socket_path;
use phux_server::ServerRuntime;

use crate::print_banner;

mod config;
pub(super) use config::build_server_config;

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

    // Panes cost descriptors; a launchd-started server inherits a soft limit
    // of 256, which a few dozen panes exhaust.
    if let Some(limit) = phux_server::daemon::raise_descriptor_limit() {
        tracing::debug!(limit, "raised the open-descriptor soft limit");
    }
    // The handoff descriptors arrive with `FD_CLOEXEC` cleared (that is how
    // they survived the exec). Mark everything close-on-exec again so the
    // next upgrade carries only what it clears itself; otherwise every
    // adopted PTY master outlives each later exec as an unowned descriptor.
    if resume.is_some() {
        phux_server::daemon::withhold_inherited_descriptors();
    }

    // Agents in panes run `phux`; make sure one resolves.
    phux_server::daemon::expose_own_cli();

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
        if let Some(error) = &slot.waiting {
            let _ = write!(extra, " (waiting to bind: {error})");
        } else if !slot.bound {
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
/// The `--autosave` archiver, refused before anything binds: a dev build
/// never writes production state.
fn open_autosave(path: Option<&Path>) -> Result<Option<phux_server::autosave::Autosave>, ExitCode> {
    let Some(path) = path else {
        return Ok(None);
    };
    phux_server::autosave::Autosave::new(
        path,
        Box::new(crate::commands::archive::AutosaveArchiver::default()),
    )
    .map(Some)
    .map_err(|err| {
        error!("server: cannot start: --autosave: {err}");
        ExitCode::FAILURE
    })
}

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
    seed_cwd: Option<&Path>,
    resume: Option<std::os::fd::RawFd>,
) -> ExitCode {
    let socket_path = match prepare_process(socket, daemonize, resume) {
        Ok(path) => path,
        Err(code) => return code,
    };

    super::startup::private_state_dir();
    let autosave = match open_autosave(autosave) {
        Ok(autosave) => autosave,
        Err(code) => return code,
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

    super::startup::shell_integration(config.defaults.shell_integration);
    super::startup::browser_shim(config.defaults.browser_shim);
    let mut cfg = build_server_config(
        session,
        &socket_path,
        config.defaults,
        config.voice,
        &config.limits,
        hook_catalog,
        seed_command,
        seed_cwd,
        exit_after_idle,
    );
    cfg.policy_mode = config.policy.mode;
    cfg.tailnet_owner = config.policy.tailnet_owner;
    cfg.restore = config.restore.clone();
    cfg.forward = config.forward.clone();
    cfg.from_programs = config.notifications.from_programs;
    cfg.plugin_startup = Some(Box::new(phux_server::plugin_startup::config_from(
        &config_loader::config_path(),
        &config.plugins,
        &phux_config::instance::state_dir(),
    )));

    let rt = match crate::commands::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    log_startup(&socket_path);
    let _audit = phux_server::audit::Recording::start();

    // Printed once the sockets are bound, so a failed start never claims
    // to be listening.
    let banner_session = session.unwrap_or("none").to_owned();
    let restore_on_start = config.restore.on_start;
    let restore_socket = socket_path.clone();
    let connector_count = connector_entries.len();
    let mut server = with_network_listeners(
        ServerRuntime::new(cfg, std::sync::Arc::new(phux_terminal::TerminalEngine)),
        listen,
        quic,
        webtransport,
    )
    .on_listening(move |listeners| {
        crate::terminal_browser::install(&socket_path);
        let extra = listener_summary(listeners, hub, connector_count, exit_after_idle);
        info!(
            "server listening on {}{extra} (session={banner_session}; Ctrl-C to stop)",
            socket_path.display(),
        );
        if restore_on_start {
            super::startup::restore_in_background(&restore_socket);
        }
    });
    if !connector_entries.is_empty() {
        server = server.connectors(connector_entries, connect);
    }
    if let Some(autosave) = autosave {
        server = server.autosave(autosave);
    }
    server = with_config_reload(server, uplinks);
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

/// Hand the runtime what each config-reload doorbell re-reads.
fn with_config_reload(
    server: phux_server::ServerRuntime,
    uplinks: Vec<phux_config::UplinkConfigEntry>,
) -> phux_server::ServerRuntime {
    // The config-reload doorbell re-reads `[[satellites]]`. A server that
    // booted with `--hub` redials the diff. One that did not still grows
    // the links when enrollment adds the first satellite (phux-lpn7.3),
    // without restarting panes.
    let server = server.hub_reload(phux_server::SatelliteSource::new(|| {
        config_loader::load()
            .map(|config| config.satellites)
            .map_err(|err| err.to_string())
    }));
    // Satellite uplinks (ADR-0182): `phux host join` writes `[[uplink]]` and
    // rings the doorbell, which starts the new uplink without a restart.
    let server = server
        .uplinks(uplinks)
        .uplink_reload(phux_server::uplink::UplinkSource::new(|| {
            config_loader::load()
                .map(|config| config.uplink)
                .map_err(|err| err.to_string())
        }));
    // Standing mirrors and producers: the config-reload doorbell re-reads
    // `[[mirrors]]` and `[[producers]]`.
    server.standing_mirrors(phux_server::runtime::MirrorSource::new(|| {
        config_loader::load()
            .map(|config| (config.mirrors, config.producers))
            .map_err(|err| err.to_string())
    }))
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
