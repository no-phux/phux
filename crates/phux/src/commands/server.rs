use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::{Duration, Instant};

use phux_config::loader as config_loader;
use phux_config::socket::{self, SocketState};
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

/// How long a client waits for the spawn lock before giving up and spawning
/// unserialised. Comfortably longer than [`AUTO_SPAWN_SOCKET_TIMEOUT`] so the
/// holder's spawn attempt can finish and be observed; short enough that a
/// stuck holder cannot hang the terminal.
const SPAWN_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Compose the fatal message printed when the server refuses to start
/// because the config file exists but failed to load: the config path,
/// the real loader error, and the remedy (`phux config check`).
///
/// One string, built once, so stderr and the server log tell the same
/// story (phux-i0e8.1.1).
fn broken_config_message(path: &Path, err: &impl std::fmt::Display) -> String {
    format!(
        "phux server: cannot start: config at {} failed to load\n  {err}\nrun: phux config check",
        path.display()
    )
}

/// Emit the server's startup info line through `tracing`, carrying
/// pid + version + socket.
///
/// Several writers can interleave into the canonical server log
/// (`$XDG_STATE_HOME/phux/server.log`): successive auto-spawned servers,
/// a service-managed server across restarts, different binary versions
/// after an upgrade. This line attributes everything that follows it to
/// one pid and one build. Emitted at `info` under the `phux` target, so
/// it passes the default filter (`phux=info,warn`) and — with the
/// auto-spawn/service stderr redirect — verifiably lands in the log file
/// (phux-i0e8.5.1).
fn log_startup(socket_path: &Path) {
    let pid = std::process::id();
    tracing::info!(
        pid,
        version = %env!("CARGO_PKG_VERSION"),
        socket = %socket_path.display(),
        "phux server started",
    );
    // phux-zomb.6: the same fact in a machine-readable place, so `phux doctor`
    // can report a crash-loop instead of leaving it buried in a rotated log.
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
/// report why the server refuses to start.
///
/// Everything here runs before the config load and the runtime bring-up, so
/// a refusal costs nothing and reads on the terminal that asked for it.
fn prepare_process(
    socket: Option<PathBuf>,
    daemonize: bool,
    resume: Option<std::os::fd::RawFd>,
) -> Result<PathBuf, ExitCode> {
    // Arm durable crash capture. This is a long-running, often daemonized
    // process whose panic has to survive in `PHUX_LOG` — nobody is watching
    // its stderr. `telemetry::init` deliberately does not install this for
    // us: it runs for every one-shot verb too, and a CLI panic logged as
    // `server panic` sends triage after a server that never faltered
    // (phux-h5hj.8).
    phux_server::telemetry::install_server_panic_hook();

    let socket_path = socket.unwrap_or_else(default_socket_path);
    // phux-iwuc: fail before the banner and the runtime bring-up when the
    // path cannot fit in a sockaddr_un — the bind inside `run_async` would
    // gate it too, but only after "listening on ..." has already printed.
    crate::commands::ensure_socket_path_fits(&socket_path)?;

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

    Ok(socket_path)
}

/// Load the one config snapshot every consumer binds from, or report why
/// the server refuses to start.
///
/// phux-i0e8.1.1: the config is loaded exactly ONCE, here. Every
/// consumer downstream (seed command, `defaults.*`, hook catalog, connector
/// registry, hub satellites) binds from this one snapshot, so an edit
/// mid-startup cannot yield a torn read. A missing file is not an
/// error — the loader returns the shipped defaults (loader.rs, `NotFound`
/// arm). A file that exists but fails to load is fatal: silently
/// disabling every configured hook and reverting scrollback/TERM/
/// window-size policy behind a normal "listening on ..." banner is
/// strictly worse than refusing to start. The connector registry's
/// security stance (malformed must not read as empty) already made a
/// parse error fatal; this makes the reported error the real one.
fn load_config() -> Result<phux_config::Config, ExitCode> {
    config_loader::load().map_err(|err| {
        let msg = broken_config_message(&config_loader::config_path(), &err);
        // Both surfaces on purpose: stderr reaches a human who
        // hand-started a foreground server; the auto-spawn path nulls
        // stdout/stdin and points stderr at a log file, so the
        // `tracing::error!` line is the durable trace either way.
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
fn build_server_config(
    session: &str,
    socket_path: &Path,
    defaults: phux_config::DefaultsCfg,
    hook_catalog: phux_server::hooks::HookCatalog,
    seed_command: Option<&str>,
    exit_after_idle: Option<u64>,
) -> ServerConfig {
    // phux-i0e8.4.1: resolve the default shell exactly once, from the
    // single config snapshot above — `defaults.shell` when set, else
    // `$SHELL`, else `/bin/sh` — and thread it into every server-owned
    // spawn path (seed session, `--seed-command`, `CreateIfMissing`,
    // `SESSION_CREATE_KEY`, command-less `SPAWN_TERMINAL`). This bind
    // must stay below the config load.
    let shell = phux_server::terminal_actor::resolve_shell(defaults.shell.as_deref());

    // phux-87rr: a server started via `phux service install`'s generated
    // launchd/systemd unit inherits the init system's minimal environment
    // — no login shell ever ran, so profile-provided `PATH` entries
    // (Homebrew, Nix) are invisible to every pane even though markers
    // like `NIX_PROFILES` may still be inherited and fool a guard into
    // thinking initialization already happened. Detect that case from
    // `SERVICE_MANAGED_ENV`, the marker `phux service install` stamps
    // into the unit's OWN environment (see `commands::service`) —
    // deliberately not sniffed from environment shape (a short `PATH`, an
    // unfamiliar parent pid): both are true of setups that never went
    // through the installer, and wrong is exactly the failure mode this
    // bug is about. Its absence is the correct default for a server a
    // human started directly from their own already-initialized
    // terminal, where re-running login-shell initialization a second
    // time is not idempotent for every setup (PATH duplication is the
    // mild failure; `nvm`/`rbenv`/`direnv` guards misfiring is not).
    let login_shell = std::env::var_os(super::service::SERVICE_MANAGED_ENV).is_some();

    // phux-07y: `--seed-command` runs that command (via `<shell> -c`) as
    // the pre-seeded session's initial program instead of a bare shell.
    // The naked-`phux` auto-spawn path passes `defaults.spawn-on-attach`
    // here; `phux new`'s auto-spawn and a hand-started `phux server`
    // pass nothing, so an explicitly-created session still gets a shell.
    // `login_shell` carries through so a service-managed server's seeded
    // command also runs with a profile-initialized `PATH` (phux-87rr).
    let seed_command = seed_command
        .map(|command| phux_server::terminal_actor::shell_command(&shell, command, login_shell));

    // `defaults.history-limit` and `defaults.history-bytes` bound each pane's
    // retained scrollback; libghostty prunes on whichever is reached first,
    // and on a wide grid that is usually the byte bound (ADR-0094).
    // `defaults.cwd-inheritance` selects how `SPAWN_TERMINAL` resolves a
    // new pane's working directory. `defaults.term` is the `TERM`
    // advertised to every server-spawned pane (a per-spawn
    // `SPAWN_TERMINAL.env` entry for `TERM` overrides it).
    // `defaults.window-size` picks the multi-client geometry policy
    // (phux-nk07).
    ServerConfig {
        socket_path: socket_path.to_path_buf(),
        pre_seeded_session: Some(session.to_owned()),
        seed_with_pty: true,
        seed_command,
        scrollback: defaults.scrollback_limits(),
        cwd_inheritance: defaults.cwd_inheritance,
        term: defaults.term,
        shell,
        login_shell,
        window_size: defaults.window_size,
        // Permissive HELLO authorization (ADR-0072): the local trust model
        // is "same OS user, kernel-enforced". phux-pjc5 installs the
        // scope-enforcing engine here for paired/remote deployments.
        policy_engine: None,
        hook_catalog,
        // Ephemeral lifetime (ADR-0063). Absent by default: the multiplexer
        // contract — live until the last pane is gone — is what a human
        // expects and is deliberately untouched.
        exit_after_idle: exit_after_idle.map(Duration::from_secs),
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
    // An ephemeral server has a lifetime a human would otherwise have to
    // infer from a flag they may not have typed themselves (a wrapper
    // script did), so the banner says so out loud.
    if let Some(secs) = exit_after_idle {
        let _ = std::fmt::Write::write_fmt(&mut extra, format_args!(" [exit-after-idle={secs}s]"));
    }
    extra
}

/// Attach the optional network listeners the flags asked for.
const fn with_network_listeners(
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

/// Build a current-thread tokio runtime and drive `ServerRuntime`
/// until Ctrl-C.
///
/// The runtime pre-seeds a session named `session` whose initial pane
/// is backed by a real PTY running the resolved default shell
/// (`defaults.shell`, falling back to `$SHELL`, then `/bin/sh`). On
/// Ctrl-C, `run_async` returns `Ok(())` and the process exits 0.
#[allow(
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools,
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
    exit_after_idle: Option<u64>,
    daemonize: bool,
    seed_command: Option<&str>,
    resume: Option<std::os::fd::RawFd>,
) -> ExitCode {
    let socket_path = match prepare_process(socket, daemonize, resume) {
        Ok(path) => path,
        Err(code) => return code,
    };

    let config = match load_config() {
        Ok(config) => config,
        Err(code) => return code,
    };

    // `[[hooks.<name>]]` entries plus enabled plugin manifests' `[[events]]`
    // feed the server-side hook dispatcher (docs/consumers/tui.md §9,
    // phux-r82.1). Relative manifest paths resolve against the config file's
    // directory.
    let hook_catalog =
        phux_server::hooks::HookCatalog::from_config(&config, &config_loader::config_path());

    // `[[satellites]]` registry, consumed below only when `--hub` was
    // asked for.
    let satellites = config.satellites;

    // Connector registry — same single snapshot; a malformed config never
    // reads as an empty registry because it never gets this far.
    let configured_connectors = config.connector;
    let connector_entries = select_connectors(configured_connectors, connect.as_deref());
    if let Err(err) = phux_server::connector::plan_connectors(&connector_entries) {
        eprintln!("phux server failed: connector: {err}");
        return ExitCode::FAILURE;
    }

    let cfg = build_server_config(
        session,
        &socket_path,
        config.defaults,
        hook_catalog,
        seed_command,
        exit_after_idle,
    );

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
        "phux server listening on {}{extra} (session={session}; Ctrl-C to stop)",
        socket_path.display()
    );
    // Attribution line for the (possibly shared) server log — see
    // `log_startup`. After the human banner so an interactive stderr
    // reads banner-first.
    log_startup(&socket_path);

    let mut server = with_network_listeners(ServerRuntime::new(cfg), listen, quic, webtransport);
    if !connector_entries.is_empty() {
        server = server.connectors(connector_entries, connect);
    }
    // Hub mode (phux-v45.1, ADR-0007): hand the `[[satellites]]` registry to
    // the runtime, which validates it into the satellite table before
    // binding. The registry comes from the same single config snapshot as
    // everything else, so a hub can never start with a silently empty
    // table: a broken config already refused to start above.
    if hub {
        server = server.hub(satellites);
    }
    if let Some(fd) = resume {
        server = server.resume(fd);
    }
    // Live rotation for the canonical server log for as long as this
    // process runs (phux-j1zj): startup-only rotation still lets one very
    // long-lived, chatty server exceed `server.log`'s size threshold
    // within a single run. Spawned directly on `rt` (not inside the
    // `block_on` future below) — a `Runtime` can be spawned onto before
    // its first `block_on`, and the task is driven by the same runtime
    // either way. It runs until `rt` itself is dropped at shutdown,
    // alongside `server.run_async`.
    rt.spawn(phux_server::telemetry::run_log_rotation_task());

    // The current-thread runtime runs every actor, pump, and client writer
    // on this thread, so this is the one call that puts the whole server's
    // keystroke path into the interactive scheduling class (ADR-0096).
    phux_server::perf::mark_started();
    report_shutdown(rt.block_on(async move { server.run_async(shutdown_signal()).await }))
}

/// Resolve when the process is asked to stop, by any route a supervisor or a
/// human actually uses.
///
/// Both signals cancel the runtime's root token, which is the *same* signal
/// idle-exit (ADR-0063) and the last-pane self-exit already deliver. That is
/// what routes the stop through the one graceful path: every pane gets
/// `TerminalActor::shutdown_pty`'s SIGHUP-then-grace-then-reap, and the socket
/// is unlinked on the way out by `unlink_socket_if_ours`.
///
/// SIGTERM is not optional politeness. Without it the process died on the
/// default disposition, so none of the above ran: pane children were left to
/// notice the kernel closing their PTY master rather than being reaped, and
/// the socket was left behind as a stale entry for the next client to trip
/// over. It also decided the exit *status*, and therefore whether a supervised
/// server stays stopped -- launchd's `KeepAlive{SuccessfulExit: false}` reads
/// death-by-signal as failure and restarts after `ThrottleInterval`, which is
/// exactly the "a deliberately stopped server stays stopped" property ADR-0080
/// claims (phux-1wka).
///
/// `phux service install --restore`'s wrapper already sends SIGTERM, so this
/// is what makes that path's `save`-then-stop actually graceful for the panes.
async fn shutdown_signal() {
    // `ctrl_c()` resolves on SIGINT *or* closure of the process's stdin
    // equivalent on some platforms; either way, treat it as "user wants out".
    let interrupt = tokio::signal::ctrl_c();

    let Ok(mut terminate) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    else {
        // Registering the handler failed (no libc slot, or a sandbox that
        // refuses). Falling back to SIGINT alone restores the previous
        // behaviour rather than refusing to serve. `eprintln!` to match this
        // command's other operator-facing lines -- for a daemonised server
        // stderr *is* the server log.
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

/// Open the canonical server log for appending, creating its parent
/// directory (the phux state dir) at mode `0o700` and the file itself at
/// mode `0o600` — the log captures operational detail that must not be
/// group/world-readable on a shared box (ADR-0028), and the state dir
/// holds TLS keys and token stores that want the same tight perms.
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

/// Environment variable that gives an auto-spawned daemon an idle backstop.
///
/// Seconds, in the same `1..=86_400` range `--exit-after-idle` accepts.
///
/// Re-exported from the crate root (and public for that reason alone): the
/// integration harness re-arms this after `env_clear()` wipes the justfile's
/// export, and a harness that spelled the name itself would keep passing
/// through a rename here while silently leaking daemons (phux-8y3o).
pub const AUTO_SPAWN_IDLE_ENV: &str = "PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE";

/// The upper bound `--exit-after-idle` accepts, mirrored here so an
/// out-of-range value is rejected by the parent with a message about the
/// variable rather than by the child with one about a flag the caller never
/// typed.
const AUTO_SPAWN_IDLE_MAX_SECS: u64 = 86_400;

/// Resolve the idle limit an auto-spawned daemon should carry, if any.
///
/// **`None` in production, and deliberately so.** Every *explicitly* spawned
/// test server passes `--exit-after-idle` as its survives-a-SIGKILLed-runner
/// backstop, but the auto-spawn path could not: nothing passed it, and the
/// child is intentionally orphaned. So an auto-spawned daemon had no owner and
/// no timer — and the last-pane self-exit is armed only once a client attaches,
/// so one that never served a client never exited either. That is the
/// structural root of the leaked-server problem (phux-nbam, phux-whhd).
///
/// The fix is an opt-in seam, not a new default. ADR-0063 and
/// `server_idle_exit::without_the_flag_an_unattended_server_stays_up`
/// deliberately pin that an unattended server stays up; making auto-spawn
/// finite would change the multiplexer contract, which is a product decision
/// and not a test-hygiene fix. This extends the mechanism ADR-0063 already
/// established rather than inventing one.
///
/// A malformed value warns and is ignored rather than failing the spawn: this
/// runs on the hot path of a naked `phux`, and refusing to start a terminal
/// because of a stray environment variable is worse than starting one without
/// a bound. It must not be *silent*, though — the whole point of setting it is
/// a bound that actually applies — so the warning rides the same `quiet` gate
/// as the auto-spawn banner (suppressed only under `--json`, whose stderr
/// contract is the error document and nothing else).
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

/// The pure half of [`auto_spawn_idle_limit`]: `None` for anything the
/// `--exit-after-idle` parser would itself reject, so the parent and the child
/// agree on what counts as a usable value.
fn parse_auto_spawn_idle(raw: &str) -> Option<u64> {
    raw.parse::<u64>()
        .ok()
        .filter(|secs| (1..=AUTO_SPAWN_IDLE_MAX_SECS).contains(secs))
}

/// Fork-exec the current binary as `phux server` (with the same
/// `--socket` override), then poll for the socket to appear.
///
/// Detachment strategy: the child is launched with `--daemonize`, so it
/// calls `setsid(2)` before binding and lands in its own session with no
/// controlling terminal — closing the launching terminal can't SIGHUP it.
/// stdin/stdout are nulled; stderr is redirected to the canonical server
/// log (`telemetry::server_log_path()`, `$XDG_STATE_HOME/phux/server.log`
/// — the same file the service unit writes) so a startup crash is
/// debuggable and `phux service logs` tails the right file for every
/// spawn path (phux-i0e8.5.1). The server never opens a tty afterward,
/// so a session-leader double-fork isn't needed.
///
/// **Lifetime.** The child is deliberately not kept as a `Child` — it owns
/// its own lifecycle — and by default it carries no idle backstop, which
/// ADR-0063 pins: an unattended server stays up, because that is the
/// multiplexer contract. `$PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE` opts a caller
/// out of that (see [`auto_spawn_idle_limit`]).
///
/// Returns `Ok` if the socket showed up within the timeout.
pub(crate) fn maybe_auto_spawn_server(
    socket_path: &Path,
    session: &str,
    seed_command: Option<&str>,
    quiet: bool,
) -> std::io::Result<()> {
    let current_exe = std::env::current_exe()?;
    let log_path = phux_server::telemetry::server_log_path();

    // The banner names the log so the one moment the user watches an
    // auto-spawn is also the moment they learn where the server writes.
    // Suppressed under `--json`, whose contract is that stderr carries the
    // error document and nothing else.
    if !quiet {
        eprintln!(
            "phux: starting server at {} (auto-spawn, session={session}; log: {})",
            socket_path.display(),
            log_path.display()
        );
    }

    // Redirect the daemon's stderr to the canonical server log so a
    // crash-on-startup is debuggable (nulled stdio leaves no trace).
    // Best-effort: fall back to /dev/null if the file can't be opened.
    let log = open_server_log(&log_path).ok();

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
    // phux-nbam: an opt-in idle backstop for this daemon. Passed as the flag
    // rather than left to the child's inherited environment so a leaked server
    // shows its own bound in `ps`, which is exactly the moment someone is
    // trying to work out why it is still running.
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

    // Spawn — we deliberately don't keep the `Child` around; the
    // server is its own lifecycle now. The OS reaps it when it exits.
    let _child = cmd.spawn()?;

    wait_until_accepting(socket_path, "auto-spawned server", &log_path)
}

/// Block until `socket_path` accepts, or the auto-spawn deadline passes.
///
/// Waiting on *accept* rather than on the socket file existing is the whole
/// point: the file exists for a window before the listener is ready, and a
/// caller that returns on `exists()` hands the user a connection refused it
/// cannot explain (phux-zomb.1).
///
/// Shared by the two paths that can put a server there — the forked daemon and
/// a service unit the init system was just asked to start — so neither can
/// drift into returning early. `what` names the one that is being waited on,
/// because "the auto-spawned server did not accept" is a misleading thing to
/// print about a supervised start.
fn wait_until_accepting(socket_path: &Path, what: &str, log_path: &Path) -> std::io::Result<()> {
    let deadline = Instant::now() + AUTO_SPAWN_SOCKET_TIMEOUT;
    loop {
        if socket::probe(socket_path) == SocketState::Live {
            return Ok(());
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

/// Ensure a server is accepting on `socket_path`, starting one if not.
///
/// This is the single entry point every client verb uses. It replaces the
/// `if !socket_path.exists() { spawn() }` gate that each call site used to
/// spell out, which had two defects that together produced the "phux is
/// wedged and I have to `rm` a socket" experience (phux-zomb.1):
///
/// * **existence is not liveness.** A server killed uncleanly leaves its
///   socket file behind. The gate then saw a file, declined to spawn, and the
///   connection failed — permanently, for every later invocation, until a
///   human removed the file. A stale entry is now detected and reaped.
/// * **no serialisation.** N concurrent invocations on a cold socket all
///   observed "no server" and all forked one. A profile-scoped advisory lock
///   now elects a single spawner; the rest wait and re-probe, and find the
///   winner's server rather than racing to bind over it.
///
/// A live server is the common case and costs one connect probe — no lock is
/// taken, so the steady state stays as cheap as the `exists()` check it
/// replaces.
///
/// # Errors
/// Returns the spawn or timeout failure. Callers report it and continue to
/// the connect attempt, which produces the user-facing remedy.
pub(crate) fn ensure_server(
    socket_path: &Path,
    session: &str,
    seed_command: Option<&str>,
    quiet: bool,
) -> std::io::Result<()> {
    if socket::probe(socket_path) == SocketState::Live {
        if !quiet {
            reconcile_version_skew(socket_path);
        }
        return Ok(());
    }

    // Serialise the spawn decision across concurrent invocations. A failure
    // to acquire the lock is never fatal: falling through to an unserialised
    // spawn is exactly the old behaviour, and the server's own bind-time
    // probe still rejects a duplicate.
    let guard = SpawnLock::acquire(&socket::spawn_lock_path(socket_path));

    // Re-probe under the lock. Whoever held it before us most likely spawned
    // the server we were about to duplicate.
    if socket::probe(socket_path) == SocketState::Live {
        return Ok(());
    }

    // Nothing is accepting. If a socket file is in the way it belonged to a
    // dead server; remove it so `bind` can succeed. `reap_stale` re-probes
    // and refuses to unlink anything live.
    if let Err(err) = socket::reap_stale(socket_path) {
        tracing::warn!(
            path = %socket_path.display(),
            error = %err,
            "could not remove stale socket entry",
        );
    }

    // The socket is free and something has to fill it. If an `--adopt` install
    // armed a unit for exactly this socket, the supervisor gets first refusal:
    // this is the moment the hand-over it was armed for becomes possible, and
    // forking our own daemon here would take the socket the unit is waiting
    // for and leave the adoption pending forever (ADR-0088).
    //
    // Only ever diverts on a *pending* adoption. A host with an ordinary
    // installed unit still auto-spawns, because reviving a server the user
    // stopped on purpose would contradict ADR-0080.
    if matches!(
        super::service::complete_pending_adoption(socket_path, quiet),
        super::service::Handover::Started
    ) {
        let result = wait_until_accepting(
            socket_path,
            "the supervised server",
            &phux_server::telemetry::server_log_path(),
        );
        drop(guard);
        return result;
    }

    let result = maybe_auto_spawn_server(socket_path, session, seed_command, quiet);
    drop(guard);
    result
}

/// Hand a server running an older build over to this one, in place.
///
/// The gap this closes (phux-zomb.7): a package manager — Homebrew, Nix, a
/// distro package — replaces the `phux` binary without telling the running
/// server, which keeps serving the old build until something kills it. Nothing
/// ever does, so the skew persists for days, and the symptoms (a client and
/// server disagreeing about behaviour that changed between builds) look like
/// random breakage. `phux update` already performs this handoff; every *other*
/// way a binary gets upgraded had no hook at all.
///
/// The handoff itself is ADR-0032's re-exec: the server passes its listening
/// fd to the new image, so panes and scrollback survive. That is what makes
/// doing this automatically defensible rather than rude — the user loses
/// nothing, and the alternative is silently talking to a stale server.
///
/// Best-effort throughout. A refused or failed upgrade leaves the old server
/// running and says so once; the attach then proceeds against it, which is
/// strictly better than refusing to work.
fn reconcile_version_skew(socket_path: &Path) {
    let ours = env!("CARGO_PKG_VERSION");
    let Some(theirs) = phux_server::health::running_version() else {
        // No history: a server from before this bookkeeping existed, or a
        // state dir that was cleared. Nothing to compare, so nothing to do.
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
            // The server re-execs and rebinds; wait for it to answer again so
            // the caller's connect does not race the handoff.
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

/// An advisory `flock` held for the duration of a spawn decision.
///
/// Scoped to the profile's runtime directory, so a dev instance and the
/// production instance never contend (phux-zomb.2). The lock file is created
/// once and never unlinked: an unlinked lock file is a new inode, and holders
/// of the old inode would no longer exclude each other.
struct SpawnLock(Option<std::fs::File>);

impl SpawnLock {
    /// Take the lock, waiting up to [`SPAWN_LOCK_TIMEOUT`].
    ///
    /// Returns an unlocked guard on any failure — a machine that cannot lock
    /// must still be able to start a terminal multiplexer.
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

    /// phux-nbam: the auto-spawn idle backstop accepts exactly what
    /// `--exit-after-idle` accepts.
    ///
    /// The parent parses the variable and passes the flag, so a value the
    /// parent waves through but the child's clap parser rejects would turn a
    /// naked `phux` into a spawn failure. Pinning both to `1..=86_400` is what
    /// keeps that from being possible.
    #[test]
    fn the_auto_spawn_idle_backstop_accepts_what_the_flag_accepts() {
        assert_eq!(parse_auto_spawn_idle("600"), Some(600));
        assert_eq!(parse_auto_spawn_idle("1"), Some(1));
        assert_eq!(parse_auto_spawn_idle("86400"), Some(86_400));

        // Rejected exactly where the flag's `range(1..=86_400)` rejects.
        assert_eq!(parse_auto_spawn_idle("0"), None);
        assert_eq!(parse_auto_spawn_idle("86401"), None);

        // And on anything that is not a whole number of seconds.
        assert_eq!(parse_auto_spawn_idle(""), None);
        assert_eq!(parse_auto_spawn_idle("600s"), None);
        assert_eq!(parse_auto_spawn_idle("1.5"), None);
        assert_eq!(parse_auto_spawn_idle("-1"), None);
        assert_eq!(parse_auto_spawn_idle("forever"), None);
    }

    #[test]
    fn broken_config_message_names_path_error_and_remedy() {
        let msg = broken_config_message(
            Path::new("/home/u/.config/phux/config.toml"),
            &"config.toml: 3:14: expected `=` after key",
        );
        // The path the user must edit.
        assert!(msg.contains("/home/u/.config/phux/config.toml"), "{msg}");
        // The real loader error, verbatim — never a misattributed one.
        assert!(
            msg.contains("config.toml: 3:14: expected `=` after key"),
            "{msg}"
        );
        // The remedy, exactly as the bead specifies it.
        assert!(msg.contains("run: phux config check"), "{msg}");
    }

    /// The startup info line carries pid + version + socket and passes the
    /// DEFAULT filter (`phux=info,warn`, `telemetry::DEFAULT_FILTER`) into a
    /// file sink — the attribution contract for the shared server log
    /// (phux-i0e8.5.1). A scoped subscriber writes to a temp file exactly
    /// like the auto-spawn stderr redirect does, so passing here means the
    /// line lands in `$XDG_STATE_HOME/phux/server.log` in production.
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
            // Mirrors telemetry::DEFAULT_FILTER (private const): the filter
            // a server gets when RUST_LOG is unset.
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

    /// The auto-spawn stderr sink opens under a `0o700` parent with the log
    /// itself at `0o600` — state-dir and log hardening (ADR-0028).
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
