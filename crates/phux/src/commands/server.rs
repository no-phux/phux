use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::{Duration, Instant};

use phux_config::loader as config_loader;
use phux_server::runtime::default_socket_path;
use phux_server::{ServerConfig, ServerRuntime};

use crate::print_banner;

/// How long the auto-spawn path waits for the freshly-launched server
/// to bind its socket before giving up. The server's bind is sub-ms on
/// a healthy system; 2s tolerates a slow-CI host without making a
/// failed spawn feel like a hang.
const AUTO_SPAWN_SOCKET_TIMEOUT: Duration = Duration::from_secs(2);

/// Poll cadence while waiting for the auto-spawned server's socket to
/// appear. 25ms is well under user-perceptible delay and small enough
/// that the typical happy path resolves in a single poll.
const AUTO_SPAWN_POLL_INTERVAL: Duration = Duration::from_millis(25);

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

/// Build a current-thread tokio runtime and drive `ServerRuntime`
/// until Ctrl-C.
///
/// The runtime pre-seeds a session named `session` whose initial pane
/// is backed by a real PTY running the user's `$SHELL` (falling back
/// to `/bin/sh`). On Ctrl-C, `run_async` returns `Ok(())` and the
/// process exits 0.
#[allow(
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools,
    clippy::too_many_lines,
    reason = "1:1 mirror of the `phux server` clap surface; bundling into a struct would just restate the clap enum"
)]
pub(crate) fn run_server(
    session: &str,
    socket: Option<PathBuf>,
    listen: Option<std::net::SocketAddr>,
    quic: Option<std::net::SocketAddr>,
    webtransport: Option<std::net::SocketAddr>,
    connect: Option<String>,
    hub: bool,
    daemonize: bool,
    seed_command: Option<&str>,
    resume: Option<std::os::fd::RawFd>,
) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    // phux-iwuc: fail before the banner and the runtime bring-up when the
    // path cannot fit in a sockaddr_un — the bind inside `run_async` would
    // gate it too, but only after "listening on ..." has already printed.
    if let Err(code) = crate::commands::ensure_socket_path_fits(&socket_path) {
        return code;
    }

    // Banner only for a hand-started foreground server (a human watching
    // a long-running process). The `--daemonize` child of the auto-spawn
    // path nulls its stdio and logs to a file, so a banner there is noise;
    // a `--resume` re-exec is likewise a detached continuation, not a
    // hand-start.
    if !daemonize && resume.is_none() {
        print_banner();
    }

    // Auto-spawn path: detach from the launching client's controlling
    // terminal so closing that terminal (SIGHUP to its session) can't
    // take the server — and the sessions it holds — down with it. The
    // client already nulled our stdio, so as a non-leader process
    // `setsid` gives us a fresh session with no controlling terminal;
    // we never open a tty afterward, so a session-leader double-fork
    // isn't needed. An `EPERM` (already a group leader) is harmless.
    if daemonize {
        let _ = rustix::process::setsid();
    }

    // phux-07y: `--seed-command` runs that command (via `$SHELL -c`) as
    // the pre-seeded session's initial program instead of a bare shell.
    // The naked-`phux` auto-spawn path passes `defaults.spawn-on-attach`
    // here; `phux new`'s auto-spawn and a hand-started `phux server`
    // pass nothing, so an explicitly-created session still gets a shell.
    let seed_command = seed_command.map(phux_server::terminal_actor::shell_command);

    // `defaults.history-limit` bounds each pane's retained scrollback.
    // A failed/absent config falls back to the schema default rather
    // than aborting startup, mirroring the other config reads here.
    let history_limit = config_loader::load().map_or_else(
        |_| phux_config::DefaultsCfg::default().history_limit,
        |cfg| cfg.defaults.history_limit,
    );

    // `defaults.cwd-inheritance` selects how `SPAWN_TERMINAL` resolves a
    // new pane's working directory. Same fallback-on-error policy as the
    // other config reads here.
    let cwd_inheritance = config_loader::load().map_or_else(
        |_| phux_config::DefaultsCfg::default().cwd_inheritance,
        |cfg| cfg.defaults.cwd_inheritance,
    );

    // `defaults.term` is the `TERM` advertised to every server-spawned
    // pane (a per-spawn `SPAWN_TERMINAL.env` entry for `TERM` overrides
    // it). Same fallback-on-error policy as the other config reads here.
    let term = config_loader::load().map_or_else(
        |_| phux_config::DefaultsCfg::default().term,
        |cfg| cfg.defaults.term,
    );

    // `defaults.window-size` picks the multi-client geometry policy
    // (phux-nk07). Same fallback-on-error policy as the other config reads.
    let window_size = config_loader::load().map_or_else(
        |_| phux_config::WindowSize::default(),
        |cfg| cfg.defaults.window_size,
    );

    // `[[hooks.<name>]]` entries plus enabled plugin manifests' `[[events]]`
    // feed the server-side hook dispatcher (docs/consumers/tui.md §9,
    // phux-r82.1). Relative manifest paths resolve against the config file's
    // directory. Same fallback-on-error policy as the other config reads.
    let hook_catalog = config_loader::load().map_or_else(
        |_| phux_server::hooks::HookCatalog::default(),
        |cfg| phux_server::hooks::HookCatalog::from_config(&cfg, &config_loader::config_path()),
    );

    // Connector configuration is security-sensitive. Unlike appearance and
    // convenience defaults above, a malformed registry must not be treated as
    // an empty registry: that would silently stop the server dialing out.
    let configured_connectors = match config_loader::load() {
        Ok(cfg) => cfg.connector,
        Err(err) => {
            eprintln!("phux server: cannot read connector registry: {err}");
            return ExitCode::FAILURE;
        }
    };
    let connector_entries = select_connectors(configured_connectors, connect.as_deref());
    if let Err(err) = phux_server::connector::plan_connectors(&connector_entries) {
        eprintln!("phux server failed: connector: {err}");
        return ExitCode::FAILURE;
    }

    let cfg = ServerConfig {
        socket_path: socket_path.clone(),
        pre_seeded_session: Some(session.to_owned()),
        seed_with_pty: true,
        seed_command,
        history_limit,
        cwd_inheritance,
        term,
        window_size,
        policy_bundle: None,
        hook_catalog,
    };

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("failed to build runtime: {err}");
            return ExitCode::FAILURE;
        }
    };

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
    if !connector_entries.is_empty() {
        let _ = std::fmt::Write::write_fmt(
            &mut extra,
            format_args!(" + connectors={}", connector_entries.len()),
        );
    }
    eprintln!(
        "phux server listening on {}{extra} (session={session}; Ctrl-C to stop)",
        socket_path.display()
    );

    let mut server = ServerRuntime::new(cfg);
    if let Some(addr) = listen {
        server = server.listen_ws(addr);
    }
    if let Some(addr) = quic {
        server = server.listen_quic(addr);
    }
    if let Some(addr) = webtransport {
        server = server.listen_webtransport(addr);
    }
    if !connector_entries.is_empty() {
        server = server.connectors(connector_entries, connect);
    }
    // Hub mode (phux-v45.1, ADR-0007): hand the `[[satellites]]` registry to
    // the runtime, which validates it into the satellite table before
    // binding. Unlike the `defaults.*` reads above, a failed config load is
    // NOT swallowed here — the user explicitly asked for a hub, and starting
    // one with a silently empty table would drop every configured satellite.
    if hub {
        let satellites = match config_loader::load() {
            Ok(cfg) => cfg.satellites,
            Err(err) => {
                eprintln!("phux server --hub: cannot read the satellite registry: {err}");
                return ExitCode::FAILURE;
            }
        };
        server = server.hub(satellites);
    }
    if let Some(fd) = resume {
        server = server.resume(fd);
    }
    let result = rt.block_on(async move {
        server
            .run_async(async {
                // tokio::signal::ctrl_c() resolves on SIGINT *or*
                // closure of the process's stdin equivalent on some
                // platforms; either way, treat it as "user wants out".
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
    });

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

/// Fork-exec the current binary as `phux server` (with the same
/// `--socket` override), then poll for the socket to appear.
///
/// Detachment strategy: the child is launched with `--daemonize`, so it
/// calls `setsid(2)` before binding and lands in its own session with no
/// controlling terminal — closing the launching terminal can't SIGHUP it.
/// stdin/stdout are nulled; stderr is redirected to `phux-server.log`
/// beside the socket so a startup crash is debuggable. The server never
/// opens a tty afterward, so a session-leader double-fork isn't needed.
///
/// Returns `Ok` if the socket showed up within the timeout.
pub(crate) fn maybe_auto_spawn_server(
    socket_path: &Path,
    session: &str,
    seed_command: Option<&str>,
) -> std::io::Result<()> {
    let current_exe = std::env::current_exe()?;

    eprintln!(
        "phux: starting server at {} (auto-spawn, session={session})",
        socket_path.display()
    );

    // Redirect the daemon's stderr to a log file next to the socket so a
    // crash-on-startup is debuggable (nulled stdio leaves no trace).
    // Best-effort: fall back to /dev/null if the file can't be opened.
    let log = socket_path
        .parent()
        .map(|dir| dir.join("phux-server.log"))
        .and_then(|p| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .ok()
        });

    let mut cmd = std::process::Command::new(current_exe);
    cmd.arg("server")
        .arg("--socket")
        .arg(socket_path)
        .arg("--session")
        .arg(session)
        .arg("--daemonize")
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    // phux-07y: forward the pre-seed command (naked `phux` passes
    // `defaults.spawn-on-attach`; other callers pass `None`).
    if let Some(seed) = seed_command {
        cmd.arg("--seed-command").arg(seed);
    }
    match log {
        Some(file) => {
            cmd.stderr(file);
        }
        None => {
            cmd.stderr(Stdio::null());
        }
    }

    // Spawn — we deliberately don't keep the `Child` around; the
    // server is its own lifecycle now. The OS reaps it when it exits.
    let _child = cmd.spawn()?;

    // Poll for the socket. The server's bind is fast (sub-ms on a
    // healthy system); the timeout exists to avoid hanging if the
    // child crashed at startup.
    let deadline = Instant::now() + AUTO_SPAWN_SOCKET_TIMEOUT;
    loop {
        if socket_path.exists() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "auto-spawned server did not bind {} within {:?}",
                    socket_path.display(),
                    AUTO_SPAWN_SOCKET_TIMEOUT
                ),
            ));
        }
        std::thread::sleep(AUTO_SPAWN_POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connector(relay: &str, token: &str) -> phux_config::ConnectorConfigEntry {
        phux_config::ConnectorConfigEntry {
            relay: relay.to_owned(),
            token_file: Some(PathBuf::from(token)),
            cert_fingerprint: Some("AB".to_owned()),
        }
    }

    #[test]
    fn configured_connectors_all_run_by_default() {
        let configured = vec![
            connector("one.example:4433", "/one"),
            connector("two.example:4433", "/two"),
        ];
        assert_eq!(select_connectors(configured.clone(), None), configured);
    }

    #[test]
    fn connect_selects_one_configured_relay_with_its_credentials() {
        let selected = select_connectors(
            vec![
                connector("one.example:4433", "/one"),
                connector("two.example:4433", "/two"),
            ],
            Some("two.example:4433"),
        );
        assert_eq!(selected, vec![connector("two.example:4433", "/two")]);
    }

    #[test]
    fn connect_allows_an_ad_hoc_loopback_relay() {
        let selected = select_connectors(Vec::new(), Some("127.0.0.1:4433"));
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].relay, "127.0.0.1:4433");
        assert!(selected[0].token_file.is_none());
        assert!(selected[0].cert_fingerprint.is_none());
    }
}
