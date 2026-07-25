use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use phux_client::attach::connection::Connection;
use phux_client::attach::{self, AttachError, CertTrust, Dial, QuicDial, WsDial};
use phux_client::predict::PredictiveConfig;
use phux_config::loader as config_loader;
use phux_protocol::wire::frame::AttachTarget;
use phux_server::runtime::default_socket_path;

use crate::commands::remote::{self, Endpoint, RemoteEntry};
use crate::commands::{DEFAULT_SESSION_NAME, print_attach_error, server::maybe_auto_spawn_server};
use crate::print_banner;

/// Naked `phux` invocation (phux-k61.1).
///
/// Per docs/consumers/tui.md §1, `phux` with no arguments is the common case: attach
/// to the user's server, lazily spawning it if it isn't running.
///
/// Resolution cascade:
///
/// 1. If the socket is missing, fork-exec ourselves as `phux server`
///    (which pre-seeds the [`DEFAULT_SESSION_NAME`] session) and wait
///    for the socket to bind. Reuses [`maybe_auto_spawn_server`].
/// 2. Attempt `ATTACH { target: Last }`. On a server with prior session
///    activity this resolves to the most-recently-focused session,
///    matching docs/consumers/tui.md §1's "attach to default session" intent.
/// 3. If `Last` is refused with no prior-attach memory (a freshly spawned
///    server, or one whose sessions were all reaped), fall back to
///    `ATTACH { target: CreateIfMissing(DEFAULT_SESSION_NAME) }`, which
///    attaches to the default session or creates it first. This is what
///    makes the auto-spawn path robust: if the server's seed pane exited
///    before we connected (the server stays alive but empty, phux-60s
///    "serve before self-exit"), this step repopulates it instead of
///    surfacing a dead-end "no session" error.
///
/// The shared cascade lives in [`attach_default_with_fallback`].
pub(crate) fn run_naked() -> ExitCode {
    // The naked invocation is a human launching their session and
    // watching it come up (possibly auto-spawning a server). One line of
    // build identity is welcome here; one-shot verbs stay silent.
    print_banner();

    let socket_path = default_socket_path();
    // phux-iwuc: a socket path over the platform's sockaddr_un limit can
    // never bind or connect — fail with the limit named, before the
    // auto-spawn below can turn it into a 2s timeout.
    if let Err(code) = super::ensure_socket_path_fits(&socket_path) {
        return code;
    }

    // phux-4li.1: name the auto-created default session from
    // `defaults.session-name-template` (e.g. `phux-${cwd-basename}`)
    // instead of the bare `DEFAULT_SESSION_NAME`. The same resolved name
    // feeds the auto-spawn seed AND the CreateIfMissing fallback so both
    // paths agree on which session to attach to.
    let default_name = resolved_default_session_name();

    if !socket_path.exists()
        && let Err(err) = maybe_auto_spawn_server(
            &socket_path,
            &default_name,
            configured_spawn_on_attach().as_deref(),
        )
    {
        eprintln!("phux: auto-spawn skipped ({err}). Start a server manually with `phux server`.");
    }

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

    let predict_cfg = match config_loader::load() {
        Ok(cfg) => PredictiveConfig {
            enabled: cfg.experimental.predictive_echo,
        },
        Err(err) => {
            eprintln!("phux: config load failed ({err}); using defaults");
            PredictiveConfig::disabled()
        }
    };

    match rt.block_on(attach_with_reconnect(
        &Dial::uds(&socket_path),
        AttachTarget::Last,
        predict_cfg,
        Some(&default_name),
    )) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            print_attach_error(&err, &socket_path, &default_name);
            ExitCode::FAILURE
        }
    }
}

/// Resolve the name for an auto-created default session from
/// `defaults.session-name-template`, substituting `${cwd-basename}`
/// against the client's current working directory (phux-4li.1).
///
/// Falls back to [`DEFAULT_SESSION_NAME`] when the config can't be
/// loaded, the cwd can't be read, or the template renders empty (e.g. a
/// `${cwd-basename}`-only template invoked from `/`).
pub(crate) fn resolved_default_session_name() -> String {
    let template = config_loader::load().map_or_else(
        |_| DEFAULT_SESSION_NAME.to_owned(),
        |cfg| cfg.defaults.session_name_template,
    );
    let cwd = std::env::current_dir().unwrap_or_default();
    let name = phux_config::render_session_name_template(&template, &cwd);
    if name.is_empty() {
        DEFAULT_SESSION_NAME.to_owned()
    } else {
        name
    }
}

/// Read `defaults.spawn-on-attach` from the on-disk config (phux-07y).
///
/// The naked-`phux` / `phux attach`-no-name auto-spawn passes this to the
/// server as the pre-seeded session's initial program. `None` (unset key
/// or unreadable config) ⇒ the seed pane runs the user's `$SHELL`.
pub(crate) fn configured_spawn_on_attach() -> Option<String> {
    config_loader::load().ok()?.defaults.spawn_on_attach
}

/// Drive one attach attempt against `socket_path` with `target`, picking
/// the predict-enabled entry point iff the user opted in. Pulled out
/// because [`run_naked`] needs to call attach twice (once for `Last`,
/// once for `ByName` fallback) and the predict/no-predict split would
/// otherwise duplicate four lines twice.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub(crate) async fn run_attach_once(
    dial: &Dial,
    target: AttachTarget,
    predict_cfg: PredictiveConfig,
) -> Result<(), AttachError> {
    // `run_with_predict_dial` with `predict.enabled = false` is identical to the
    // non-predictive path, so one call covers both transports and both modes.
    attach::run_with_predict_dial(dial, target, predict_cfg).await
}

/// Attach to the user's default session with the naked-`phux` fallback
/// cascade: try `Last`; if the server has no prior-attach memory, fall
/// back to `CreateIfMissing(default)`.
///
/// The `CreateIfMissing` step is what makes the cascade robust to an
/// *empty* server — e.g. one whose auto-spawned seed pane exited before
/// any client attached. The server stays alive (phux-60s only self-exits
/// after it has served a client), and this step creates a fresh default
/// session and attaches, rather than dead-ending on "session not found".
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub(crate) async fn attach_default_with_fallback(
    dial: &Dial,
    default_name: &str,
    predict_cfg: PredictiveConfig,
) -> Result<(), AttachError> {
    match run_attach_once(dial, AttachTarget::Last, predict_cfg).await {
        Ok(()) => Ok(()),
        Err(AttachError::Refused(message)) => {
            eprintln!(
                "phux: no prior-attach session (server said: {message}); creating `{default_name}`"
            );
            run_attach_once(dial, default_create_target(default_name), predict_cfg).await
        }
        Err(err) => Err(err),
    }
}

fn default_create_target(default_name: &str) -> AttachTarget {
    AttachTarget::CreateIfMissing {
        name: default_name.to_owned(),
        command: None,
        // Seed the pane in the client's cwd so tools whose persistence is
        // keyed by project directory, such as `claude --resume`, find it.
        cwd: client_cwd(),
    }
}

/// The client's current working directory as a wire `cwd` value
/// (phux-0db).
///
/// Every session-create path sends this instead of `cwd: None` so the
/// seed pane lands in the user's project directory rather than the
/// daemon's CWD — `None` made the PTY child inherit wherever the server
/// process happened to start (typically `$HOME`), which broke tools
/// whose persistence is keyed by directory (e.g. `claude --resume`).
/// `None` only when the cwd is unreadable; the server validates the path
/// and falls back to its default spawn directory if it isn't an
/// enterable directory on the server host.
pub(crate) fn client_cwd() -> Option<String> {
    std::env::current_dir()
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

/// How long a vanished server is given to come back before the client gives up
/// and exits. A graceful upgrade (ADR-0032) re-execs in well under a second;
/// the generous window only matters for a server that crashed and won't return.
const RECONNECT_DEADLINE: Duration = Duration::from_secs(10);
/// Poll cadence while waiting for the re-exec'd server to start accepting.
const RECONNECT_POLL: Duration = Duration::from_millis(100);

/// Drive an attach, transparently reconnecting if the server *vanishes*
/// mid-session — the graceful-upgrade blink (ADR-0032): the re-exec'd server
/// keeps the socket bound, so the client re-attaches and the `ATTACH`
/// handshake resyncs the screen via `TERMINAL_SNAPSHOT`.
///
/// A clean detach returns `Ok`. An [`AttachError::Disconnected`] (server closed
/// without `DETACHED`) triggers a bounded reconnect: if the socket starts
/// accepting again within [`RECONNECT_DEADLINE`] we re-attach; if the socket
/// file is gone (a clean shutdown unlinks it) or never accepts again, the
/// disconnect is surfaced. `default_name = Some` drives the naked-`phux`
/// `Last` + `CreateIfMissing` cascade each attempt; `None` re-attaches `target`
/// directly.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
async fn attach_with_reconnect(
    dial: &Dial,
    target: AttachTarget,
    predict_cfg: PredictiveConfig,
    default_name: Option<&str>,
) -> Result<(), AttachError> {
    loop {
        let result = match default_name {
            Some(name) => attach_default_with_fallback(dial, name, predict_cfg).await,
            None => run_attach_once(dial, target.clone(), predict_cfg).await,
        };
        match result {
            Ok(()) => return Ok(()),
            Err(AttachError::Disconnected) => {
                if wait_until_connectable(dial, RECONNECT_DEADLINE).await {
                    eprintln!("phux: server restarted; re-attaching…");
                } else {
                    return Err(AttachError::Disconnected);
                }
            }
            Err(other) => return Err(other),
        }
    }
}

/// Wait until the server accepts again on `dial`, or give up.
///
/// Returns `true` as soon as a fresh connection succeeds (the re-exec'd server
/// is up), and `false` once `deadline` elapses while connections keep failing
/// (e.g. a crashed server). For UDS it short-circuits to `false` if the socket
/// file is gone — a clean shutdown unlinks it, so there is nothing to reconnect
/// to; a graceful upgrade never removes the socket, so it falls into the
/// retry-until-connectable path. Remote transports probe by completing a real
/// dial and dropping it, the transport analogue of the UDS connect-and-drop
/// probe.
async fn wait_until_connectable(dial: &Dial, deadline: Duration) -> bool {
    let end = Instant::now() + deadline;
    loop {
        let connectable = match dial {
            Dial::Uds(path) => {
                if !path.exists() {
                    return false;
                }
                tokio::net::UnixStream::connect(path).await.is_ok()
            }
            Dial::Quic(quic) => match Connection::connect_quic(quic).await {
                // Close the probe cleanly so the server reaps it now; otherwise
                // each 100ms probe during a restart would leave a phantom
                // connection alive until the idle timeout.
                Ok(conn) => {
                    conn.shutdown().await;
                    true
                }
                Err(_) => false,
            },
            Dial::Ws(ws) => match Connection::connect_ws(ws).await {
                Ok(conn) => {
                    conn.shutdown().await;
                    true
                }
                Err(_) => false,
            },
        };
        if connectable {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        tokio::time::sleep(RECONNECT_POLL).await;
    }
}

/// Block on the tokio current-thread runtime, drive the attach loop,
/// translate the result into a process exit code.
///
/// If the socket isn't there (or refuses connections), this also
/// attempts a best-effort auto-spawn of `phux server` before
/// connecting — see [`maybe_auto_spawn_server`].
/// Attach through a registered `[[remote]]` entry (ADR-0055).
///
/// The registry supplies the endpoint, the pin, and the token, so the
/// operator types a name instead of two 64-hex strings. `session` overrides
/// the entry's own pinned session when the caller named one.
///
/// `ssh://` re-execs `ssh -t HOST phux attach` rather than dialing: there is
/// no consumer-side ssh transport (`Dial` is QUIC or WebSocket), and there
/// does not need to be — the session still lives on the remote server and
/// still survives the connection dropping.
pub(crate) fn run_attach_remote(entry: &RemoteEntry, session: Option<String>) -> ExitCode {
    let endpoint = match Endpoint::parse(&entry.endpoint) {
        Ok(endpoint) => endpoint,
        Err(err) => {
            eprintln!("phux: remote {:?}: {err}", entry.name);
            return ExitCode::FAILURE;
        }
    };
    let session = session.or_else(|| entry.session.clone());

    let token = match remote::read_token(entry) {
        Ok(token) => token,
        Err(err) => {
            eprintln!("phux: remote {:?}: {err}", entry.name);
            return ExitCode::FAILURE;
        }
    };

    match endpoint {
        Endpoint::Quic(target) => {
            run_attach_quic(session, target, token, entry.cert_fingerprint.clone(), None)
        }
        Endpoint::Ws(url) => {
            run_attach_ws(session, url, token, entry.cert_fingerprint.clone(), None)
        }
        Endpoint::Ssh(host) => run_attach_over_ssh(&host, session.as_deref()),
    }
}

/// Replace this process with `ssh -t HOST phux attach [SESSION]`.
///
/// `exec` rather than spawn-and-wait so the operator's terminal, signals, and
/// exit code belong to ssh directly — an intermediate parent would only add a
/// process that mangles Ctrl-C. `-t` forces a TTY, which the interactive
/// attach requires.
fn run_attach_over_ssh(host: &str, session: Option<&str>) -> ExitCode {
    use std::os::unix::process::CommandExt as _;

    let program = std::env::var_os("PHUX_SSH").unwrap_or_else(|| "ssh".into());
    let mut command = std::process::Command::new(&program);
    command.arg("-t").arg(host).arg("phux").arg("attach");
    if let Some(session) = session {
        command.arg(session);
    }

    // `exec` only returns on failure.
    let err = command.exec();
    eprintln!("phux: could not exec {}: {err}", program.to_string_lossy());
    ExitCode::FAILURE
}

pub(crate) fn run_attach(session: Option<String>, socket: Option<PathBuf>) -> ExitCode {
    // A name in the registry is a deliberate operator statement — they ran
    // `phux enroll` or `phux remote add` for it — so it wins over the
    // local-session reading of the same word. `--socket` is an explicit
    // local intent and suppresses the lookup.
    if socket.is_none()
        && let Some(name) = session.as_deref()
        && let Some(entry) = remote::find(name)
    {
        return run_attach_remote(&entry, None);
    }

    let socket_path = socket.unwrap_or_else(default_socket_path);
    // phux-iwuc: fail before auto-spawn with the sockaddr_un limit named,
    // instead of the 2s spawn timeout + a doomed connect.
    if let Err(code) = super::ensure_socket_path_fits(&socket_path) {
        return code;
    }
    // Resolve the session name to pass through to auto-spawn before we
    // move `session` into the AttachTarget. With no explicit name this
    // path behaves like naked `phux`, so it resolves the same
    // `session-name-template` (phux-4li.1) rather than the bare
    // DEFAULT_SESSION_NAME — keeping the auto-spawn seed and the
    // create-and-attach fallback on one agreed name.
    let default_name = resolved_default_session_name();
    let session_for_spawn = session.clone().unwrap_or_else(|| default_name.clone());
    // phux-07y: only the no-name (naked-`phux`-equivalent) case seeds with
    // `defaults.spawn-on-attach`. An explicit `phux attach NAME` is like
    // `phux new`: its auto-spawned seed pane gets a plain shell.
    let seed_command = if session.is_none() {
        configured_spawn_on_attach()
    } else {
        None
    };
    let target = session.map_or(AttachTarget::Last, AttachTarget::ByName);

    // Best-effort: if no socket exists, fork-exec ourselves into a
    // detached server. Failures here are non-fatal — the subsequent
    // `attach::run` call will surface the connect error.
    //
    // `phux-roz` (4): the spawned server is pre-seeded with the same
    // session name the user is trying to attach to, so the subsequent
    // `ATTACH` doesn't refuse with "session not found" against a
    // surprise `default` session.
    if !socket_path.exists() {
        match maybe_auto_spawn_server(&socket_path, &session_for_spawn, seed_command.as_deref()) {
            Ok(()) => {}
            Err(err) => {
                eprintln!(
                    "phux: auto-spawn skipped ({err}). Start a server manually with `phux server`."
                );
            }
        }
    }

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

    // Load user config to discover experimental opt-ins. Failures here
    // are non-fatal — we log and fall back to defaults so a syntax
    // error in config.toml doesn't lock the user out of their server.
    let predict_cfg = match config_loader::load() {
        Ok(cfg) => PredictiveConfig {
            enabled: cfg.experimental.predictive_echo,
        },
        Err(err) => {
            eprintln!("phux: config load failed ({err}); using defaults");
            PredictiveConfig::disabled()
        }
    };

    // No explicit name → behave like naked `phux`: try `Last`, then
    // create-and-attach the default session. This is robust to an empty
    // server (e.g. one whose auto-spawned seed pane exited before we
    // connected). An explicit name attaches to that session only.
    let dial = Dial::uds(&socket_path);
    let result = match target {
        AttachTarget::Last => rt.block_on(attach_with_reconnect(
            &dial,
            AttachTarget::Last,
            predict_cfg,
            Some(&default_name),
        )),
        other => rt.block_on(attach_with_reconnect(&dial, other, predict_cfg, None)),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // `phux-roz` (5): produce actionable text per variant. The
            // guard (if any) has already dropped, so this lands on the
            // cooked terminal.
            print_attach_error(&err, &socket_path, &session_for_spawn);
            ExitCode::FAILURE
        }
    }
}

/// Stderr hint appended when a non-loopback dial got no answer at all —
/// the failure mode of an overlay network (Tailscale/WireGuard) that is
/// down on either end. Six-space continuation indent matches the `phux:`
/// multi-line hint convention above.
const OVERLAY_REACHABILITY_HINT: &str = "      The server did not answer or its name could not be resolved; credentials were never checked.\n      If the host lives on an overlay network (Tailscale/WireGuard), confirm the overlay is up on both ends.";

/// Decide whether a failed attach earns [`OVERLAY_REACHABILITY_HINT`]:
/// only a reachability failure ([`AttachError::Unreachable`]) on a
/// non-loopback target. Pin and auth failures ([`AttachError::Connect`])
/// mean a host answered, so the hint would mislead; loopback never
/// involves an overlay.
fn reachability_hint(err: &AttachError, loopback: bool) -> Option<&'static str> {
    (!loopback && matches!(err, AttachError::Unreachable(_))).then_some(OVERLAY_REACHABILITY_HINT)
}

/// Split a `--quic` `HOST:PORT` dial target into host and port. HOST may be
/// a DNS name, an IPv4 literal, or a bracketed IPv6 literal (`[::1]:8788`);
/// brackets stay on the host half for the caller to trim.
fn split_host_port(target: &str) -> Result<(&str, u16), String> {
    let (host, port) = target.rsplit_once(':').ok_or_else(|| {
        format!("--quic target '{target}' is missing a port (expected HOST:PORT)")
    })?;
    if host.is_empty() {
        return Err(format!(
            "--quic target '{target}' is missing a host (expected HOST:PORT)"
        ));
    }
    let port = port
        .parse::<u16>()
        .map_err(|err| format!("--quic target '{target}' has an invalid port: {err}"))?;
    Ok((host, port))
}

/// Split and resolve a `--quic` `HOST:PORT` target to its first address,
/// alongside the default TLS server name for the dial. Prints the failure —
/// plus [`OVERLAY_REACHABILITY_HINT`] when a DNS name failed to resolve, the
/// `MagicDNS`-down shape of an overlay outage — and returns the failure exit
/// code on error.
///
/// Resolution happens before the trust decision on purpose: the
/// loopback-vs-routable choice keys on the **resolved** address.
/// Multi-address fallback is out of scope — the first resolved address wins.
fn resolve_quic_target(
    rt: &tokio::runtime::Runtime,
    target: &str,
) -> Result<(std::net::SocketAddr, String), ExitCode> {
    let (host, port) = match split_host_port(target) {
        Ok(parts) => parts,
        Err(err) => {
            eprintln!("phux: {err}");
            return Err(ExitCode::FAILURE);
        }
    };
    let bare_host = host.trim_matches(['[', ']']);
    let host_is_ip_literal = bare_host.parse::<std::net::IpAddr>().is_ok();

    let resolved = rt
        .block_on(tokio::net::lookup_host((bare_host, port)))
        .map(|mut addrs| addrs.next());
    let failure = match resolved {
        Ok(Some(addr)) => {
            // The TLS server name defaults to the dialed hostname when one
            // was given (conventional SNI); an IP-literal target keeps the
            // historical `localhost` default, matching the server's
            // self-signed SANs.
            let server_name = if host_is_ip_literal {
                "localhost".to_owned()
            } else {
                bare_host.to_owned()
            };
            return Ok((addr, server_name));
        }
        Ok(None) => "name resolution returned no addresses".to_owned(),
        Err(err) => format!("name resolution failed: {err}"),
    };
    eprintln!("phux: QUIC attach to {target} failed: {failure}");
    // Only a DNS name reaches here (an IP literal resolves without touching
    // DNS), and a name that fails to resolve is the overlay-down
    // reachability failure — MagicDNS unreachable when Tailscale is stopped
    // on this end — so it earns the same hint an unanswered dial does.
    if !host_is_ip_literal {
        eprintln!("{OVERLAY_REACHABILITY_HINT}");
    }
    Err(ExitCode::FAILURE)
}

/// Attach over QUIC (`phux-y8v6`, ADR-0007) to a `phux server --quic`
/// listener at `target` (`HOST:PORT`; a DNS name — e.g. a Tailscale `MagicDNS`
/// name — resolves before dialing, mirroring the hub's satellite dialer).
///
/// Unlike the UDS path there is no auto-spawn — the server lives on another
/// host (or another address) and the user points at it explicitly. TLS trust is
/// resolved up front, keyed on the **resolved** address:
///
/// * an explicit `--cert-fingerprint` pins the server's leaf certificate (the
///   value `phux pair` prints), the trust anchor for any routable host;
/// * a target resolving to **loopback** with no fingerprint falls back to
///   skip-verify (local dev — TLS still runs, but there is no untrusted
///   network path to MITM);
/// * a target resolving to a **routable** address with no fingerprint is
///   refused, rather than silently trusting whatever certificate answers.
///
/// With no session name this runs the same `Last` → `CreateIfMissing` cascade
/// the naked path does; an explicit name attaches to that session only.
#[allow(
    clippy::needless_pass_by_value,
    reason = "clap hands over the owned HOST:PORT value; a &str signature would only push the borrow into main.rs's dispatch"
)]
pub(crate) fn run_attach_quic(
    session: Option<String>,
    target: String,
    token: Option<String>,
    cert_fingerprint: Option<String>,
    server_name: Option<String>,
) -> ExitCode {
    print_banner();

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

    let (addr, default_server_name) = match resolve_quic_target(&rt, &target) {
        Ok(resolved) => resolved,
        Err(code) => return code,
    };

    let trust = match cert_fingerprint {
        Some(fingerprint) => CertTrust::Pinned(fingerprint),
        None if addr.ip().is_loopback() => CertTrust::SkipVerify,
        None => {
            eprintln!(
                "phux: refusing to dial non-loopback QUIC server {target} without --cert-fingerprint."
            );
            eprintln!(
                "      Run `phux pair` on the server host to print its certificate fingerprint,"
            );
            eprintln!("      then pass it: phux attach --quic {target} --cert-fingerprint <FP>");
            return ExitCode::FAILURE;
        }
    };

    let token = match token {
        Some(token) => match attach::quic::parse_token_hex(&token) {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                eprintln!("phux: {err}");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };

    let dial = Dial::Quic(QuicDial {
        addr,
        server_name: server_name.unwrap_or(default_server_name),
        token,
        trust,
    });

    let predict_cfg = match config_loader::load() {
        Ok(cfg) => PredictiveConfig {
            enabled: cfg.experimental.predictive_echo,
        },
        Err(err) => {
            eprintln!("phux: config load failed ({err}); using defaults");
            PredictiveConfig::disabled()
        }
    };

    let default_name = resolved_default_session_name();
    let (attach_target, default) = session.map_or_else(
        || (AttachTarget::Last, Some(default_name.as_str())),
        |name| (AttachTarget::ByName(name), None),
    );

    match rt.block_on(attach_with_reconnect(
        &dial,
        attach_target,
        predict_cfg,
        default,
    )) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("phux: QUIC attach to {target} failed: {err}");
            if let Some(hint) = reachability_hint(&err, addr.ip().is_loopback()) {
                eprintln!("{hint}");
            }
            ExitCode::FAILURE
        }
    }
}

/// Attach over WebSocket to `phux server --listen`.
pub(crate) fn run_attach_ws(
    session: Option<String>,
    url: String,
    token: Option<String>,
    cert_fingerprint: Option<String>,
    tls_server_name: Option<String>,
) -> ExitCode {
    print_banner();

    let target = match attach::ws::WsTarget::parse(&url) {
        Ok(target) => target,
        Err(err) => {
            eprintln!("phux: {err}");
            return ExitCode::FAILURE;
        }
    };

    if !target.secure && !target.is_loopback() {
        eprintln!("phux: refusing plaintext WebSocket attach to non-loopback URL {url}.");
        eprintln!("      Use wss:// plus `phux pair` credentials for remote devices.");
        return ExitCode::FAILURE;
    }
    if target.secure && !target.is_loopback() && cert_fingerprint.is_none() {
        eprintln!(
            "phux: refusing to dial non-loopback WebSocket server {url} without --cert-fingerprint."
        );
        eprintln!("      Run `phux pair` on the server host, then pass the printed fingerprint.");
        return ExitCode::FAILURE;
    }
    if target.secure && !target.is_loopback() && token.is_none() {
        eprintln!("phux: refusing remote WebSocket attach to {url} without --token.");
        eprintln!("      Run `phux pair` on the server host and pass the printed token once.");
        return ExitCode::FAILURE;
    }

    // Captured before `target` is shadowed by the AttachTarget below; the
    // failure hint needs to know whether the dial left the machine.
    let loopback = target.is_loopback();

    let token = match token {
        Some(token) => match attach::quic::parse_token_hex(&token) {
            Ok(_) => Some(token.trim().to_owned()),
            Err(err) => {
                eprintln!("phux: {err}");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };

    let trust = cert_fingerprint.map_or(CertTrust::SkipVerify, CertTrust::Pinned);
    let dial = Dial::Ws(WsDial {
        url,
        token,
        trust,
        tls_server_name,
    });

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

    let predict_cfg = match config_loader::load() {
        Ok(cfg) => PredictiveConfig {
            enabled: cfg.experimental.predictive_echo,
        },
        Err(err) => {
            eprintln!("phux: config load failed ({err}); using defaults");
            PredictiveConfig::disabled()
        }
    };

    let default_name = resolved_default_session_name();
    let (target, default) = session.map_or_else(
        || (AttachTarget::Last, Some(default_name.as_str())),
        |name| (AttachTarget::ByName(name), None),
    );

    match rt.block_on(attach_with_reconnect(&dial, target, predict_cfg, default)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("phux: WebSocket attach failed: {err}");
            if let Some(hint) = reachability_hint(&err, loopback) {
                eprintln!("{hint}");
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_create_target_carries_client_cwd() {
        let expected = std::env::current_dir()
            .expect("test cwd")
            .to_string_lossy()
            .into_owned();
        let target = default_create_target("default");

        assert_eq!(
            target,
            AttachTarget::CreateIfMissing {
                name: "default".to_owned(),
                command: None,
                cwd: Some(expected),
            }
        );
    }

    /// The reconnect probe returns fast for a missing socket (clean shutdown),
    /// and `true` once a listener is bound (the re-exec'd server is up).
    #[tokio::test]
    async fn reconnect_probe_distinguishes_missing_and_live_sockets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("probe.sock");

        // No socket file: nothing to reconnect to — returns without waiting.
        let start = Instant::now();
        assert!(!wait_until_connectable(&Dial::uds(&path), Duration::from_secs(5)).await);
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "a missing socket should fail fast, not burn the deadline"
        );

        // A bound listener: connectable.
        let _listener = tokio::net::UnixListener::bind(&path).expect("bind");
        assert!(wait_until_connectable(&Dial::uds(&path), Duration::from_secs(2)).await);
    }

    /// The overlay hint fires only for a reachability failure on a
    /// non-loopback target — never for pin/auth failures (a host that
    /// answered) and never for loopback (no overlay involved).
    #[test]
    fn reachability_hint_gates_on_variant_and_loopback() {
        let unreachable = AttachError::Unreachable("x".to_owned());
        assert_eq!(
            reachability_hint(&unreachable, false),
            Some(OVERLAY_REACHABILITY_HINT)
        );
        assert_eq!(reachability_hint(&unreachable, true), None);

        let pin_mismatch = AttachError::Connect(
            "server certificate fingerprint mismatch (pinned AA, got BB)".to_owned(),
        );
        assert_eq!(reachability_hint(&pin_mismatch, false), None);

        let io = AttachError::Io(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        assert_eq!(reachability_hint(&io, false), None);
    }

    /// `--quic` targets split on the last `:`, so IPv4 literals, bracketed
    /// IPv6 literals, and DNS names all parse; a missing or malformed port
    /// is rejected up front with a usage error.
    #[test]
    fn split_host_port_accepts_documented_target_shapes() {
        assert_eq!(
            split_host_port("127.0.0.1:8788"),
            Ok(("127.0.0.1", 8788_u16))
        );
        assert_eq!(split_host_port("[::1]:1"), Ok(("[::1]", 1_u16)));
        assert_eq!(
            split_host_port("myhost.tailnet-name.ts.net:8788"),
            Ok(("myhost.tailnet-name.ts.net", 8788_u16))
        );

        let missing_port = split_host_port("myhost.tailnet-name.ts.net");
        assert!(
            missing_port
                .as_ref()
                .is_err_and(|err| err.contains("missing a port")),
            "got {missing_port:?}"
        );
        let bad_port = split_host_port("myhost:notaport");
        assert!(
            bad_port
                .as_ref()
                .is_err_and(|err| err.contains("invalid port")),
            "got {bad_port:?}"
        );
        let missing_host = split_host_port(":8788");
        assert!(
            missing_host
                .as_ref()
                .is_err_and(|err| err.contains("missing a host")),
            "got {missing_host:?}"
        );
    }
}
