use std::cell::RefCell;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

use phux_client::attach::connection::Connection;
use phux_client::attach::record::SessionRecorder;
use phux_client::attach::status_bar::Notice;
use phux_client::attach::{self, AttachEnd, AttachError, CertTrust, Dial, QuicDial, WsDial};
use phux_client::predict::PredictiveConfig;
use phux_config::loader as config_loader;
use phux_protocol::wire::frame::AttachTarget;
use phux_record::cast::CastVersion;
use phux_server::runtime::default_socket_path;

use crate::commands::rec::RecordSpec;
use crate::commands::remote::{self, Endpoint, RemoteEntry};
use crate::commands::{DEFAULT_SESSION_NAME, print_attach_error, server::ensure_server};

/// A live `--rec` recorder, shared between reconnect attempts.
///
/// `Rc<RefCell<..>>` and not a plain `&mut` because the tee lives inside the
/// driver's render sink for the duration of one attach, and a graceful-upgrade
/// reconnect (ADR-0032) starts a *second* attach against the *same* recording.
type RecorderHandle = Rc<RefCell<SessionRecorder>>;

/// Refuse interactive entry points before they can start a server, connect,
/// or let the driver write terminal-control sequences.
pub(crate) fn interactive_tty_preflight() -> Result<(), ExitCode> {
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        return Ok(());
    }
    eprintln!("phux: interactive use requires both stdin and stdout to be terminals");
    Err(ExitCode::FAILURE)
}

/// phux-i0e8.2.2: explain how a successful attach ended, once the TUI is
/// down and stdout/stderr are the user's cooked terminal again.
///
/// A plain detach says nothing (the quiet, expected ending); a last-pane
/// death prints its one-line explanation so an OOM-killed shell does not
/// look like a phux crash. On the production UDS/QUIC/WS paths the driver
/// already prints this line inside its own teardown (`exit_after_detach`
/// exits the process before control returns here — see its doc comment);
/// this helper covers every path that DOES return an [`AttachEnd`], and
/// keeps both CLI callers (`phux attach` / `phux new`) on one wording.
pub(crate) fn report_attach_end(end: AttachEnd) {
    if let Some(line) = end.explanation() {
        eprintln!("{line}");
    }
}

/// Export the `--rec` capture and report it, once the TUI is down.
///
/// Deliberately after `attach_with_reconnect` returns and before any error
/// reporting: raw mode is restored and the alt screen is gone, so stdout is
/// the user's terminal again. A recording is worth reporting even when the
/// attach itself ended badly — the bytes up to the failure are on disk and
/// playable.
fn finalize_recording(rec: Option<&RecordSpec>) {
    if let Some(spec) = rec {
        crate::commands::rec::finalize(spec);
    }
}

/// Naked `phux` invocation (phux-k61.1).
///
/// Per docs/consumers/tui.md §1, `phux` with no arguments is the common case: attach
/// to the user's server, lazily spawning it if it isn't running.
///
/// Resolution cascade:
///
/// 1. If the socket is missing, fork-exec ourselves as `phux server`
///    (which pre-seeds the [`DEFAULT_SESSION_NAME`] session) and wait
///    for the socket to accept. Reuses [`ensure_server`], which also
///    reaps a socket left behind by a server that died uncleanly.
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
pub(crate) fn run_naked(socket: Option<PathBuf>, rec: Option<&RecordSpec>) -> ExitCode {
    // No build banner on any attach path (phux-i0e8.10.1): the TUI
    // raises the alt screen almost immediately, wiping the line before a
    // human can read it. The banner stays on the long-running foreground
    // entry points (`phux server`, `phux relay run`), whose stderr
    // remains visible.

    if let Err(code) = interactive_tty_preflight() {
        return code;
    }

    let socket_path = socket.unwrap_or_else(default_socket_path);
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

    if let Err(err) = ensure_server(
        &socket_path,
        &default_name,
        configured_spawn_on_attach().as_deref(),
        false,
    ) {
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

    let result = rt.block_on(attach_with_reconnect(
        &Dial::uds(&socket_path),
        AttachTarget::Last,
        predict_cfg,
        Some(&default_name),
        rec,
    ));
    finalize_recording(rec);
    match result {
        Ok(end) => {
            report_attach_end(end);
            ExitCode::SUCCESS
        }
        // `Disconnected` can only leave `attach_with_reconnect` through the
        // reconnect window, which already printed the distinct SocketGone /
        // TimedOut report (both naming `phux doctor`); a second remedy block
        // here would double-print.
        Err(AttachError::Disconnected) => ExitCode::FAILURE,
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
) -> Result<AttachEnd, AttachError> {
    run_attach_once_rec(dial, target, predict_cfg, None, None).await
}

/// [`run_attach_once`] with an optional live recorder and an optional
/// attach-time status-bar notice.
///
/// Split from the plain form so callers that cannot record — `phux new`,
/// which attaches as the tail of a create — are not forced to pass a `None`
/// that means nothing to them. `initial_notice` (phux-i0e8.2.3) is the
/// reconnect loop's "re-attached after server restart", shown inside the
/// next attach's TUI; every first attach passes `None`.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub(crate) async fn run_attach_once_rec(
    dial: &Dial,
    target: AttachTarget,
    predict_cfg: PredictiveConfig,
    rec: Option<RecorderHandle>,
    initial_notice: Option<Notice>,
) -> Result<AttachEnd, AttachError> {
    // `run_with_predict_dial` with `predict.enabled = false` is identical to the
    // non-predictive path, so one call covers both transports and both modes.
    // The recorded entry point differs only in wrapping the driver's render
    // sink with the tee, so the two branches share every other behaviour.
    match rec {
        Some(rec) => {
            attach::run_recorded_dial(dial, target, predict_cfg, rec, initial_notice).await
        }
        None => attach::run_with_predict_dial(dial, target, predict_cfg, initial_notice).await,
    }
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
    rec: Option<&RecorderHandle>,
    initial_notice: Option<Notice>,
) -> Result<AttachEnd, AttachError> {
    match run_attach_once_rec(
        dial,
        AttachTarget::Last,
        predict_cfg,
        rec.map(Rc::clone),
        initial_notice.clone(),
    )
    .await
    {
        Ok(end) => Ok(end),
        Err(AttachError::Refused(message)) => {
            eprintln!(
                "phux: no prior-attach session (server said: {message}); creating `{default_name}`"
            );
            // The notice rides the fallback attempt too: whichever attempt
            // actually attaches is the one whose TUI should announce the
            // reconnect.
            run_attach_once_rec(
                dial,
                default_create_target(default_name),
                predict_cfg,
                rec.map(Rc::clone),
                initial_notice,
            )
            .await
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
/// without `DETACHED`) triggers a bounded reconnect, visible on the cooked
/// terminal as a live per-second countdown (phux-i0e8.2.3): if the socket
/// starts accepting again within [`RECONNECT_DEADLINE`] we re-attach — with a
/// status-bar notice inside the new TUI announcing the recovery; if the socket
/// file is gone (a clean shutdown unlinks it) or never accepts again, the two
/// distinct failure reports are printed HERE (both naming `phux doctor`) and
/// `Err(Disconnected)` is returned — call sites must NOT print a second remedy
/// block for it. `default_name = Some` drives the naked-`phux` `Last` +
/// `CreateIfMissing` cascade each attempt; `None` re-attaches `target`
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
    rec: Option<&RecordSpec>,
) -> Result<AttachEnd, AttachError> {
    // Created ONCE, outside the loop, and cloned into each attempt: a
    // graceful-upgrade reconnect must continue the SAME recording. Creating
    // it per attempt would truncate the cast at every server hot-swap, which
    // is precisely the moment a user most wants the recording to be intact.
    // The file is opened here, on the cooked terminal, so a bad path is an
    // ordinary CLI error rather than a failure behind the alt screen.
    let recorder: Option<RecorderHandle> = match rec {
        // v2 and not v3: v3 is not backward compatible, and every consumer
        // that reads v3 also reads v2 (ADR-0060). The interactive surface has
        // no version knob, so the interoperable one is the only defensible
        // choice; `phux rec --cast-version 3` is where a user opts in.
        Some(spec) => Some(Rc::new(RefCell::new(SessionRecorder::create(
            &spec.cast_path,
            None,
            CastVersion::V2,
        )?))),
        None => None,
    };

    // phux-i0e8.2.3: set after a successful reconnect so the NEXT attach's
    // status bar announces the recovery inside the live TUI. A cooked-
    // terminal eprintln here is alt-screened over within milliseconds, so
    // the in-TUI notice is the visible surface; `take()` per attempt keeps
    // a later, unrelated re-attach from re-announcing an old restart.
    let mut initial_notice: Option<Notice> = None;
    let outcome = loop {
        let result = match default_name {
            Some(name) => {
                attach_default_with_fallback(
                    dial,
                    name,
                    predict_cfg,
                    recorder.as_ref(),
                    initial_notice.take(),
                )
                .await
            }
            None => {
                run_attach_once_rec(
                    dial,
                    target.clone(),
                    predict_cfg,
                    recorder.as_ref().map(Rc::clone),
                    initial_notice.take(),
                )
                .await
            }
        };
        match result {
            Ok(end) => break Ok(end),
            Err(AttachError::Disconnected) => {
                // The RawModeGuard dropped on the unwind out of the attach,
                // so this whole window runs on the cooked primary screen —
                // an honest, visible countdown instead of ~10 s of blank
                // terminal (phux-i0e8.2.3).
                eprintln!(
                    "phux: lost the server connection; waiting up to {}s for it to come back",
                    RECONNECT_DEADLINE.as_secs()
                );
                match wait_with_countdown(dial, RECONNECT_DEADLINE).await {
                    ReconnectOutcome::Connectable => {
                        eprintln!("phux: server is back; re-attaching…");
                        initial_notice = Some(Notice::info(RECONNECT_NOTICE_TEXT));
                    }
                    outcome @ (ReconnectOutcome::SocketGone | ReconnectOutcome::TimedOut) => {
                        // Fully reported here — the call sites map a
                        // `Disconnected` breaking out of this loop straight
                        // to the failure exit code without a second remedy
                        // block (see `run_naked` / `run_attach`).
                        for line in reconnect_failure_lines(outcome, RECONNECT_DEADLINE) {
                            eprintln!("{line}");
                        }
                        break Err(AttachError::Disconnected);
                    }
                }
            }
            Err(other) => break Err(other),
        }
    };

    close_recorder(recorder);
    outcome
}

/// The status-bar notice text a post-reconnect attach shows (phux-i0e8.2.3).
const RECONNECT_NOTICE_TEXT: &str = "re-attached after server restart";

/// How the bounded reconnect probe ended (phux-i0e8.2.3).
///
/// Three-way rather than a bool because the two failure shapes mean
/// different things to the user: a *gone* socket is a server that shut
/// down cleanly (a clean shutdown unlinks it — nothing is coming back),
/// while a socket that exists but never accepts within the deadline is a
/// server that crashed or hung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReconnectOutcome {
    /// The server accepts connections again — re-attach now.
    Connectable,
    /// The UDS socket file disappeared: the server shut down cleanly and
    /// is not restarting. Only reachable on UDS dials; remote transports
    /// have no socket file to observe.
    SocketGone,
    /// The deadline elapsed with every probe still failing.
    TimedOut,
}

/// One line of `\r`-overwritten countdown, pure so tests can pin the
/// format. `remaining` is rounded UP to whole seconds so the countdown
/// starts at the full deadline and never shows `0s` while still waiting.
fn reconnect_progress_line(remaining: Duration) -> String {
    let secs = remaining
        .saturating_add(Duration::from_millis(999))
        .as_secs();
    format!("phux: reconnecting… {secs}s left (Ctrl-C to give up)")
}

/// The cooked-terminal failure report for a reconnect window that closed
/// without a server (phux-i0e8.2.3). Pure so tests can pin both shapes;
/// each names its distinct cause and ends with the `phux doctor` remedy.
///
/// Only the two failure outcomes are meaningful here; `Connectable` never
/// reaches this function on the production path and maps to an empty
/// report rather than a panic.
fn reconnect_failure_lines(outcome: ReconnectOutcome, deadline: Duration) -> Vec<String> {
    match outcome {
        ReconnectOutcome::Connectable => Vec::new(),
        ReconnectOutcome::SocketGone => vec![
            "phux: the server shut down (its socket is gone) and is not restarting".to_owned(),
            "  start a new one with `phux` (attaches, auto-starting a server) or `phux server`"
                .to_owned(),
            "  run `phux doctor` for a health check".to_owned(),
        ],
        ReconnectOutcome::TimedOut => vec![
            format!(
                "phux: the server did not come back within {}s — it may have crashed",
                deadline.as_secs()
            ),
            format!(
                "  server log: {}",
                phux_server::telemetry::server_log_path().display()
            ),
            "  run `phux doctor` for a health check".to_owned(),
        ],
    }
}

/// Drive [`wait_until_connectable`] while painting a `\r`-overwritten
/// per-second countdown on stderr, so the reconnect window is visible
/// while it happens rather than after it succeeded.
///
/// The countdown line is erased (`\r` + EL) before returning, so whatever
/// the caller prints next starts on a clean line.
async fn wait_with_countdown(dial: &Dial, deadline: Duration) -> ReconnectOutcome {
    let end = Instant::now() + deadline;
    let mut probe = std::pin::pin!(wait_until_connectable(dial, deadline));
    // The first tick fires immediately, so the countdown appears at the
    // full deadline before the first probe can even fail.
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            outcome = &mut probe => {
                eprint!("\r\x1b[K");
                return outcome;
            }
            _ = ticker.tick() => {
                let remaining = end.saturating_duration_since(Instant::now());
                eprint!("\r\x1b[K{}", reconnect_progress_line(remaining));
            }
        }
    }
}

/// Flush the residual UTF-8 tail, backfill `duration`, and close the cast.
///
/// Idempotent because the production clean-detach path finalizes immediately
/// before `process::exit`; this remains the fallback for returning error paths.
/// Diagnostics go through `tracing` (file-only on the client) and never to
/// stderr, which the TUI has only just released.
fn close_recorder(recorder: Option<RecorderHandle>) {
    let Some(handle) = recorder else {
        return;
    };
    if let Err(err) = handle.borrow_mut().finish_in_place() {
        tracing::warn!(error = %err, "closing the session recording failed");
    }
}

/// Wait until the server accepts again on `dial`, or give up.
///
/// Returns [`ReconnectOutcome::Connectable`] as soon as a fresh connection
/// succeeds (the re-exec'd server is up), and [`ReconnectOutcome::TimedOut`]
/// once `deadline` elapses while connections keep failing (e.g. a crashed
/// server). For UDS it short-circuits to [`ReconnectOutcome::SocketGone`] if
/// the socket file is gone — a clean shutdown unlinks it, so there is nothing
/// to reconnect to; a graceful upgrade never removes the socket, so it falls
/// into the retry-until-connectable path. Remote transports probe by
/// completing a real dial and dropping it, the transport analogue of the UDS
/// connect-and-drop probe.
async fn wait_until_connectable(dial: &Dial, deadline: Duration) -> ReconnectOutcome {
    let end = Instant::now() + deadline;
    loop {
        let connectable = match dial {
            Dial::Uds(path) => {
                if !path.exists() {
                    return ReconnectOutcome::SocketGone;
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
            return ReconnectOutcome::Connectable;
        }
        if Instant::now() >= end {
            return ReconnectOutcome::TimedOut;
        }
        tokio::time::sleep(RECONNECT_POLL).await;
    }
}

/// Block on the tokio current-thread runtime, drive the attach loop,
/// translate the result into a process exit code.
///
/// If the socket isn't there (or refuses connections), this also
/// attempts a best-effort auto-spawn of `phux server` before
/// connecting — see [`ensure_server`].
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
pub(crate) fn run_attach_remote(
    entry: &RemoteEntry,
    session: Option<String>,
    rec: Option<&RecordSpec>,
) -> ExitCode {
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
        Endpoint::Quic(target) => run_attach_quic(
            session,
            target,
            token,
            entry.cert_fingerprint.clone(),
            None,
            rec,
        ),
        Endpoint::Ws(url) => run_attach_ws(
            session,
            url,
            token,
            entry.cert_fingerprint.clone(),
            None,
            rec,
        ),
        // `exec`s into ssh, so this process — and any recorder it holds —
        // ceases to exist here. Recording a `ssh://` remote means running
        // `phux --rec` on the far side.
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

/// `phux attach [NAME]` with no recording.
///
/// Kept as a distinct entry point so callers that can never record — the
/// worktree verbs, which attach as the tail of a create — do not have to
/// carry a `None` they cannot ever populate.
pub(crate) fn run_attach(session: Option<String>, socket: Option<PathBuf>) -> ExitCode {
    run_attach_rec(session, socket, None)
}

pub(crate) fn run_attach_rec(
    session: Option<String>,
    socket: Option<PathBuf>,
    rec: Option<&RecordSpec>,
) -> ExitCode {
    if let Err(code) = interactive_tty_preflight() {
        return code;
    }

    // A name in the registry is a deliberate operator statement — they ran
    // `phux host enroll` or `phux host add` for it — so it wins over the
    // local-session reading of the same word. `--socket` is an explicit
    // local intent and suppresses the lookup.
    if socket.is_none()
        && let Some(name) = session.as_deref()
        && let Some(entry) = remote::find(name)
    {
        return run_attach_remote(&entry, None, rec);
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

    // Best-effort: if nothing is accepting, fork-exec ourselves into a
    // detached server. Failures here are non-fatal — the subsequent
    // attach driver call will surface the connect error.
    //
    // `phux-roz` (4): the spawned server is pre-seeded with the same
    // session name the user is trying to attach to, so the subsequent
    // `ATTACH` doesn't refuse with "session not found" against a
    // surprise `default` session.
    if let Err(err) = ensure_server(
        &socket_path,
        &session_for_spawn,
        seed_command.as_deref(),
        false,
    ) {
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
            rec,
        )),
        other => rt.block_on(attach_with_reconnect(&dial, other, predict_cfg, None, rec)),
    };
    finalize_recording(rec);
    let exit = match result {
        Ok(end) => {
            report_attach_end(end);
            ExitCode::SUCCESS
        }
        // Already reported by the reconnect window (distinct SocketGone /
        // TimedOut lines naming `phux doctor`) — see `attach_with_reconnect`.
        Err(AttachError::Disconnected) => ExitCode::FAILURE,
        Err(err) => {
            // `phux-roz` (5): produce actionable text per variant. The
            // guard (if any) has already dropped, so this lands on the
            // cooked terminal.
            print_attach_error(&err, &socket_path, &session_for_spawn);
            ExitCode::FAILURE
        }
    };
    // `tokio::io::stdin` delegates reads to a blocking-pool task. An attach
    // whose transport disappears while stdin is idle has no way to cancel
    // that OS read; dropping the runtime would therefore wait forever and
    // leave the CLI stuck after it has already restored the terminal.
    rt.shutdown_timeout(Duration::ZERO);
    exit
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
    rec: Option<&RecordSpec>,
) -> ExitCode {
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

    let result = rt.block_on(attach_with_reconnect(
        &dial,
        attach_target,
        predict_cfg,
        default,
        rec,
    ));
    finalize_recording(rec);
    match result {
        Ok(end) => {
            report_attach_end(end);
            ExitCode::SUCCESS
        }
        // Already reported by the reconnect window (distinct SocketGone /
        // TimedOut lines naming `phux doctor`) — see `attach_with_reconnect`.
        Err(AttachError::Disconnected) => ExitCode::FAILURE,
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
    rec: Option<&RecordSpec>,
) -> ExitCode {
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

    let result = rt.block_on(attach_with_reconnect(
        &dial,
        target,
        predict_cfg,
        default,
        rec,
    ));
    finalize_recording(rec);
    match result {
        Ok(end) => {
            report_attach_end(end);
            ExitCode::SUCCESS
        }
        // Already reported by the reconnect window (distinct SocketGone /
        // TimedOut lines naming `phux doctor`) — see `attach_with_reconnect`.
        Err(AttachError::Disconnected) => ExitCode::FAILURE,
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
    use phux_protocol::wire::frame::DetachReason;

    use super::*;

    /// phux-i0e8.2.2: the one-line ending explanation both CLI callers
    /// (`phux attach`, `phux new`) print after teardown. A detach says
    /// nothing; a last-pane death names the exit shape; the process exit
    /// code stays SUCCESS either way (the callers map `Ok(_)` to
    /// `ExitCode::SUCCESS` unconditionally).
    #[test]
    fn attach_end_explanation_covers_all_shapes() {
        assert_eq!(
            AttachEnd::Detached { reason: None }.explanation(),
            None,
            "a plain detach needs no words"
        );
        assert_eq!(
            AttachEnd::Detached {
                reason: Some(DetachReason::Requested),
            }
            .explanation(),
            None,
            "a detach the user asked for needs no words either"
        );
        // phux-l83x: every other reason is an ending the user did not
        // choose, and before the DETACHED payload existed all of them were
        // indistinguishable from the quiet case above.
        assert_eq!(
            AttachEnd::Detached {
                reason: Some(DetachReason::ServerShutdown),
            }
            .explanation()
            .as_deref(),
            Some("phux: detached: the server is shutting down"),
        );
        assert_eq!(
            AttachEnd::Detached {
                reason: Some(DetachReason::Replaced),
            }
            .explanation()
            .as_deref(),
            Some("phux: detached: another client took over this attach"),
        );
        assert_eq!(
            AttachEnd::LastPaneClosed {
                exit_status: Some(0)
            }
            .explanation()
            .as_deref(),
            Some("phux: session ended: the last pane exited 0"),
        );
        assert_eq!(
            AttachEnd::LastPaneClosed {
                exit_status: Some(137)
            }
            .explanation()
            .as_deref(),
            Some("phux: session ended: the last pane exited 137"),
        );
        assert_eq!(
            AttachEnd::LastPaneClosed { exit_status: None }
                .explanation()
                .as_deref(),
            Some("phux: session ended: the last pane killed (signal or unknown)"),
        );
    }

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

    /// phux-i0e8.2.3: the reconnect probe is three-way. A missing socket
    /// (clean shutdown) is `SocketGone`, and fast; a bound listener (the
    /// re-exec'd server is up) is `Connectable`; a path that exists but
    /// never accepts (a crashed/hung server) burns the deadline into
    /// `TimedOut`.
    #[tokio::test]
    async fn reconnect_probe_distinguishes_gone_live_and_dead_sockets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("probe.sock");

        // No socket file: nothing to reconnect to — returns without waiting.
        let start = Instant::now();
        assert_eq!(
            wait_until_connectable(&Dial::uds(&path), Duration::from_secs(5)).await,
            ReconnectOutcome::SocketGone
        );
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "a missing socket should fail fast, not burn the deadline"
        );

        // A bound listener: connectable.
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        assert_eq!(
            wait_until_connectable(&Dial::uds(&path), Duration::from_secs(2)).await,
            ReconnectOutcome::Connectable
        );
        drop(listener);

        // A path that exists but never accepts: the deadline elapses.
        std::fs::remove_file(&path).ok();
        std::fs::File::create(&path).expect("plug the socket path");
        assert_eq!(
            wait_until_connectable(&Dial::uds(&path), Duration::from_millis(300)).await,
            ReconnectOutcome::TimedOut
        );
    }

    /// phux-i0e8.2.3: the countdown line is `\r`-overwritten in place, so
    /// the format is pinned exactly; remaining time rounds UP to whole
    /// seconds so it opens at the full deadline and never reads `0s`
    /// mid-wait.
    #[test]
    fn reconnect_progress_line_rounds_up_and_pins_format() {
        assert_eq!(
            reconnect_progress_line(Duration::from_secs(10)),
            "phux: reconnecting… 10s left (Ctrl-C to give up)"
        );
        assert_eq!(
            reconnect_progress_line(Duration::from_millis(9_100)),
            "phux: reconnecting… 10s left (Ctrl-C to give up)"
        );
        assert_eq!(
            reconnect_progress_line(Duration::from_millis(200)),
            "phux: reconnecting… 1s left (Ctrl-C to give up)"
        );
        assert_eq!(
            reconnect_progress_line(Duration::ZERO),
            "phux: reconnecting… 0s left (Ctrl-C to give up)"
        );
    }

    /// phux-i0e8.2.3: the two reconnect-window failure shapes are distinct
    /// sentences — a gone socket is a clean shutdown, a timeout is a crash
    /// — and BOTH name `phux doctor` as the remedy.
    #[test]
    fn reconnect_failure_lines_are_distinct_and_name_doctor() {
        let deadline = Duration::from_secs(10);

        let gone = reconnect_failure_lines(ReconnectOutcome::SocketGone, deadline);
        assert_eq!(
            gone[0],
            "phux: the server shut down (its socket is gone) and is not restarting"
        );
        assert!(
            gone.iter().any(|l| l.contains("phux doctor")),
            "SocketGone must name phux doctor: {gone:?}"
        );

        let timed_out = reconnect_failure_lines(ReconnectOutcome::TimedOut, deadline);
        assert_eq!(
            timed_out[0],
            "phux: the server did not come back within 10s — it may have crashed"
        );
        assert!(
            timed_out.iter().any(|l| l.contains("phux doctor")),
            "TimedOut must name phux doctor: {timed_out:?}"
        );
        assert!(
            timed_out.iter().any(|l| l.starts_with("  server log: ")),
            "TimedOut points at the server log (the crash reason lives there): {timed_out:?}"
        );

        assert_ne!(gone[0], timed_out[0], "the two failures read differently");
        assert!(
            reconnect_failure_lines(ReconnectOutcome::Connectable, deadline).is_empty(),
            "a successful reconnect has nothing to report"
        );
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
