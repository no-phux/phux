use std::cell::RefCell;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

use phux_client::attach::connection::Connection;
use phux_client::attach::{
    AttachEnd, AttachError, CertTrust, Dial, InputReplayJournal, QuicDial, WsDial,
};
use phux_client::predict::PredictiveConfig;
use phux_client_runtime::reconnect::Ladder;
use phux_config::loader as config_loader;
use phux_protocol::wire::frame::AttachTarget;
use phux_record::cast::CastVersion;
use phux_server::runtime::default_socket_path;
use phux_tui::attach::{self, record::SessionRecorder, status_bar::Notice};

use crate::commands::rec::RecordSpec;
use crate::commands::remote::{self, Endpoint, RemoteEntry};
use crate::commands::{DEFAULT_SESSION_NAME, print_attach_error, server::ensure_server};

/// A live `--rec` recorder, shared so a graceful-upgrade reconnect continues
/// the same recording.
type RecorderHandle = Rc<RefCell<SessionRecorder>>;

/// The ADR-0053 acknowledged-input replay journal, shared across reconnect
/// attempts so a journaled operation is replayed under its original id. Remote
/// dials only.
type ReplayHandle = Rc<RefCell<InputReplayJournal>>;

/// Refuse interactive entry points before they can start a server, connect,
/// or let the driver write terminal-control sequences.
pub(crate) fn interactive_tty_preflight() -> Result<(), ExitCode> {
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        return Ok(());
    }
    eprintln!("phux: interactive use requires both stdin and stdout to be terminals");
    Err(ExitCode::FAILURE)
}

/// Explain how a successful attach ended, once the terminal is cooked again:
/// a detach says nothing, a last-pane death prints one line. Covers the paths
/// that return an [`AttachEnd`] rather than exiting inside the driver.
pub(crate) fn report_attach_end(end: AttachEnd) {
    if let Some(line) = end.explanation() {
        eprintln!("{line}");
    }
}

/// Export the `--rec` capture and report it, once the TUI is down. Runs even
/// when the attach ended badly: the bytes up to the failure are playable.
fn finalize_recording(rec: Option<&RecordSpec>) {
    if let Some(spec) = rec {
        crate::commands::rec::finalize(spec);
    }
}

/// Naked `phux`: attach to the user's server, auto-spawning it if the socket
/// is missing, and send `ATTACH { target: Last }`. A refusal from an older
/// server permits one lookup-only `ByName` retry; attach never creates.
pub(crate) fn run_naked(socket: Option<PathBuf>, rec: Option<&RecordSpec>) -> ExitCode {
    // No build banner on attach paths: the alt screen would wipe it.

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

    // Resolve the auto-spawn seed from `defaults.session-name-template`.
    // After startup the server owns this identity; ATTACH sends only `Last`.
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

    let dial = Dial::uds(&socket_path);
    let predict_cfg = predictive_config_for(&dial);

    let result = rt.block_on(attach_with_reconnect(
        &dial,
        AttachTarget::Last,
        predict_cfg,
        fallback_lookup_name(&default_name),
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

/// `name` as the lookup-only retry for a server that cannot resolve
/// `Last`, or `None` when the template draws `${random-name}`: a fresh
/// pick names no existing session, so retrying it would only print a name
/// nobody chose (phux-c2td.6).
fn fallback_lookup_name(name: &str) -> Option<&str> {
    let template = configured_session_name_template();
    (!phux_config::template_has_random_name(&template)).then_some(name)
}

/// Resolve the auto-created default session's name from
/// `defaults.session-name-template` (`${cwd-basename}`, `${random-name}`),
/// falling back to [`DEFAULT_SESSION_NAME`] when the config or cwd is
/// unreadable or the template renders empty.
pub(crate) fn resolved_default_session_name() -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    render_default_session_name(
        &configured_session_name_template(),
        &cwd,
        &mut phux_config::NameRng::from_entropy(),
    )
}

/// The configured `defaults.session-name-template`, or
/// [`DEFAULT_SESSION_NAME`] when the config can't be loaded.
pub(crate) fn configured_session_name_template() -> String {
    config_loader::load().map_or_else(
        |_| DEFAULT_SESSION_NAME.to_owned(),
        |cfg| cfg.defaults.session_name_template,
    )
}

/// Render `template` against `cwd` with `rng` for `${random-name}`,
/// falling back to [`DEFAULT_SESSION_NAME`] when it renders empty.
pub(crate) fn render_default_session_name(
    template: &str,
    cwd: &std::path::Path,
    rng: &mut phux_config::NameRng,
) -> String {
    let name = phux_config::render_session_name_template_with(template, cwd, rng);
    if name.is_empty() {
        DEFAULT_SESSION_NAME.to_owned()
    } else {
        name
    }
}

/// Read `defaults.spawn-on-attach`: the auto-spawned seed pane's program.
/// `None` runs the user's `$SHELL`.
pub(crate) fn configured_spawn_on_attach() -> Option<String> {
    config_loader::load().ok()?.defaults.spawn_on_attach
}

/// Build the sole attach target for an optional session selector.
///
/// An omitted name remains `Last`; the server owns default-seed resolution.
/// This path never upgrades an attach into `CreateIfMissing`.
fn requested_attach_target(session: Option<String>) -> AttachTarget {
    session.map_or(AttachTarget::Last, AttachTarget::ByName)
}

/// Build the compatibility fallback for a server that cannot resolve an
/// untouched `Last`. The fallback can only look up an existing session.
fn default_lookup_target(default_name: &str) -> AttachTarget {
    AttachTarget::ByName(default_name.to_owned())
}

/// Drive one attach attempt against `socket_path` with `target`, picking
/// the predict-enabled entry point iff the user opted in.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub(crate) async fn run_attach_once(
    dial: &Dial,
    target: AttachTarget,
    predict_cfg: PredictiveConfig,
) -> Result<AttachEnd, AttachError> {
    run_attach_once_rec(dial, target, predict_cfg, None, None, None).await
}

/// [`run_attach_once`] with an optional live recorder and an optional
/// status-bar notice (the reconnect loop's "re-attached" message).
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
    input_replay: Option<ReplayHandle>,
) -> Result<AttachEnd, AttachError> {
    // `run_with_predict_dial` with `predict.enabled = false` is identical to the
    // non-predictive path, so one call covers both transports and both modes.
    // The recorded entry point differs only in wrapping the driver's render
    // sink with the tee, so the two branches share every other behaviour.
    match rec {
        Some(rec) => {
            attach::run_recorded_dial(dial, target, predict_cfg, rec, initial_notice, input_replay)
                .await
        }
        None => {
            attach::run_with_predict_dial(dial, target, predict_cfg, initial_notice, input_replay)
                .await
        }
    }
}

#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
async fn run_attach_connection_rec(
    connection: Connection,
    dial: &Dial,
    target: AttachTarget,
    predict_cfg: PredictiveConfig,
    rec: Option<RecorderHandle>,
    initial_notice: Option<Notice>,
    input_replay: Option<ReplayHandle>,
) -> Result<AttachEnd, AttachError> {
    match rec {
        Some(rec) => {
            Box::pin(attach::run_recorded_connection(
                connection,
                dial,
                target,
                predict_cfg,
                rec,
                initial_notice,
                input_replay,
            ))
            .await
        }
        None => {
            Box::pin(attach::run_with_predict_connection(
                connection,
                dial,
                target,
                predict_cfg,
                initial_notice,
                input_replay,
            ))
            .await
        }
    }
}

/// Attach to the user's default session via `Last`; if an older server
/// refuses it, make exactly one lookup-only retry by name (never
/// `CreateIfMissing`).
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
    input_replay: Option<&ReplayHandle>,
) -> Result<AttachEnd, AttachError> {
    match run_attach_once_rec(
        dial,
        AttachTarget::Last,
        predict_cfg,
        rec.map(Rc::clone),
        initial_notice.clone(),
        input_replay.map(Rc::clone),
    )
    .await
    {
        Ok(end) => Ok(end),
        Err(AttachError::Refused(message)) => {
            eprintln!(
                "phux: server could not resolve the last session ({message}); trying existing `{default_name}`"
            );
            run_attach_once_rec(
                dial,
                default_lookup_target(default_name),
                predict_cfg,
                rec.map(Rc::clone),
                initial_notice,
                input_replay.map(Rc::clone),
            )
            .await
        }
        Err(err) => Err(err),
    }
}

#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
async fn attach_default_with_connection_fallback(
    connection: Connection,
    dial: &Dial,
    default_name: &str,
    predict_cfg: PredictiveConfig,
    rec: Option<&RecorderHandle>,
    initial_notice: Option<Notice>,
    input_replay: Option<&ReplayHandle>,
) -> Result<AttachEnd, AttachError> {
    match Box::pin(run_attach_connection_rec(
        connection,
        dial,
        AttachTarget::Last,
        predict_cfg,
        rec.map(Rc::clone),
        initial_notice.clone(),
        input_replay.map(Rc::clone),
    ))
    .await
    {
        Ok(end) => Ok(end),
        Err(AttachError::Refused(message)) => {
            eprintln!(
                "phux: server could not resolve the last session ({message}); trying existing `{default_name}`"
            );
            run_attach_once_rec(
                dial,
                default_lookup_target(default_name),
                predict_cfg,
                rec.map(Rc::clone),
                initial_notice,
                input_replay.map(Rc::clone),
            )
            .await
        }
        Err(err) => Err(err),
    }
}

struct AttachAttempt<'a> {
    dial: &'a Dial,
    target: &'a AttachTarget,
    predict_cfg: PredictiveConfig,
    default_name: Option<&'a str>,
    recorder: Option<&'a RecorderHandle>,
    input_replay: Option<&'a ReplayHandle>,
}

impl AttachAttempt<'_> {
    #[allow(
        clippy::future_not_send,
        reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
    )]
    async fn run(
        &self,
        connection: Option<Box<Connection>>,
        initial_notice: Option<Notice>,
    ) -> Result<AttachEnd, AttachError> {
        match (connection, self.default_name) {
            (Some(connection), Some(name)) => {
                Box::pin(attach_default_with_connection_fallback(
                    *connection,
                    self.dial,
                    name,
                    self.predict_cfg,
                    self.recorder,
                    initial_notice,
                    self.input_replay,
                ))
                .await
            }
            (Some(connection), None) => {
                Box::pin(run_attach_connection_rec(
                    *connection,
                    self.dial,
                    self.target.clone(),
                    self.predict_cfg,
                    self.recorder.map(Rc::clone),
                    initial_notice,
                    self.input_replay.map(Rc::clone),
                ))
                .await
            }
            (None, Some(name)) => {
                Box::pin(attach_default_with_fallback(
                    self.dial,
                    name,
                    self.predict_cfg,
                    self.recorder,
                    initial_notice,
                    self.input_replay,
                ))
                .await
            }
            (None, None) => {
                Box::pin(run_attach_once_rec(
                    self.dial,
                    self.target.clone(),
                    self.predict_cfg,
                    self.recorder.map(Rc::clone),
                    initial_notice,
                    self.input_replay.map(Rc::clone),
                ))
                .await
            }
        }
    }
}

/// The client's cwd as a wire `cwd`, so a seed pane starts in the user's
/// project directory rather than the daemon's. `None` only when unreadable; the
/// server validates it and falls back to its default.
pub(crate) fn client_cwd() -> Option<String> {
    std::env::current_dir()
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

/// How long a vanished server is given to come back and how hard to poll,
/// per dial lane (see [`reconnect_policy`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReconnectPolicy {
    /// How long to keep trying before giving up and exiting.
    deadline: Duration,
    /// The cadence between attempts (the first is immediate): the runtime's
    /// ladder for this lane (ADR-0133), so the arithmetic exists once.
    ladder: Ladder,
}

/// The local lane: the ADR-0032 graceful-upgrade blink. The re-exec'd server
/// keeps the socket bound and returns in well under a second; ten seconds lets a
/// real crash report promptly.
const UDS_RECONNECT: ReconnectPolicy = ReconnectPolicy {
    deadline: Duration::from_secs(10),
    ladder: Ladder::LOCAL_UPGRADE,
};

/// The remote lanes: usually the client's network changing (wifi to
/// cellular, sleep/wake), which routinely takes longer than ten seconds. Each
/// probe is a full TLS handshake, so the runtime's exponential interactive
/// ladder replaces a flat poll.
const REMOTE_RECONNECT: ReconnectPolicy = ReconnectPolicy {
    deadline: Duration::from_secs(60),
    ladder: Ladder::INTERACTIVE,
};

/// Which policy governs this dial.
const fn reconnect_policy(dial: &Dial) -> ReconnectPolicy {
    match dial {
        Dial::Uds(_) => UDS_RECONNECT,
        Dial::Quic(_) | Dial::Ws(_) => REMOTE_RECONNECT,
    }
}

/// Drive an attach, reconnecting if the server vanishes mid-session (the
/// ADR-0032 blink). A clean detach returns `Ok`. On `Disconnected` a live
/// countdown runs on the cooked terminal; a recovery re-attaches with a
/// status-bar notice, and a gone or never-accepting socket prints its failure
/// report HERE and returns `Err(Disconnected)`, so callers must not print a
/// second remedy. `default_name = Some` enables one lookup-only `ByName` retry
/// after a refused `Last`.
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
    // ADR-0140: a local-socket attach is this machine. A registered remote
    // recorded its name before dialing; an ad-hoc `--quic`/`--ws` dial
    // records nothing, so its sidebar shows only the attached server.
    if matches!(dial, Dial::Uds(_)) {
        phux_tui::attach::hosts::set_attach_origin(phux_tui::attach::hosts::AttachOrigin::Local);
    }
    // Created once, outside the loop: a reconnect must continue the SAME
    // recording, and a bad path is reported on the cooked terminal.
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

    // ADR-0053: one replay journal per invocation, so an operation unresolved
    // when the socket died is resent under its idempotent id. Remote lanes only;
    // the UDS blink restarts the server with a fresh incarnation.
    let input_replay: Option<ReplayHandle> = match dial {
        Dial::Uds(_) => None,
        Dial::Quic(_) | Dial::Ws(_) => Some(Rc::new(RefCell::new(InputReplayJournal::new()))),
    };

    // Set after a successful reconnect so the next attach's status bar
    // announces the recovery (a cooked eprintln would be alt-screened over).
    let mut initial_notice: Option<Notice> = None;
    let mut reconnect_connection = None;
    let attempt = AttachAttempt {
        dial,
        target: &target,
        predict_cfg,
        default_name,
        recorder: recorder.as_ref(),
        input_replay: input_replay.as_ref(),
    };
    let outcome = loop {
        let result =
            Box::pin(attempt.run(reconnect_connection.take(), initial_notice.take())).await;
        match result {
            Ok(end) => break Ok(end),
            Err(AttachError::Disconnected) => {
                // The in-flight correlation died with the socket; the
                // journaled operations themselves survive for the next
                // attempt to re-decide.
                if let Some(journal) = input_replay.as_ref() {
                    journal.borrow_mut().connection_lost();
                }
                // The RawModeGuard dropped on the unwind out of the attach,
                // so this whole window runs on the cooked primary screen —
                // an honest, visible countdown instead of ~10 s of blank
                // terminal (phux-i0e8.2.3).
                let policy = reconnect_policy(dial);
                eprintln!(
                    "phux: lost the server connection; waiting up to {}s for it to come back",
                    policy.deadline.as_secs()
                );
                match wait_with_countdown(dial, policy).await {
                    ReconnectOutcome::Connectable(connection) => {
                        eprintln!("phux: server is back; re-attaching…");
                        // Those two lines live on the primary screen. The alt
                        // screen hides them until quit, which is when they
                        // reappear under the prompt.
                        erase_reconnect_banner();
                        initial_notice = Some(Notice::info(RECONNECT_NOTICE_TEXT));
                        reconnect_connection = connection;
                    }
                    outcome @ (ReconnectOutcome::SocketGone
                    | ReconnectOutcome::TimedOut
                    | ReconnectOutcome::Refused(_)) => {
                        // Fully reported here — the call sites map a
                        // `Disconnected` breaking out of this loop straight
                        // to the failure exit code without a second remedy
                        // block (see `run_naked` / `run_attach`).
                        for line in reconnect_failure_lines(&outcome, policy.deadline) {
                            eprintln!("{line}");
                        }
                        break Err(AttachError::Disconnected);
                    }
                }
            }
            Err(other) => break Err(other),
        }
    };
    drop(reconnect_connection);

    // ADR-0053: whatever is still journaled when the loop gives up resolves
    // here, on the cooked terminal: an attempted paste is unknown, a never-sent one
    // is a safe refusal.
    if let Some(journal) = input_replay.as_ref() {
        for report in journal
            .borrow_mut()
            .drain_unresolved("the connection could not be re-established")
        {
            eprintln!("phux: {}", report.notice_line());
        }
    }

    close_recorder(recorder);
    outcome
}

/// The status-bar notice text a post-reconnect attach shows (phux-i0e8.2.3).
const RECONNECT_NOTICE_TEXT: &str = "re-attached after server restart";

/// How the bounded reconnect probe ended. A gone socket is a clean shutdown,
/// one that never accepts is a crash or hang, and a refusal is a healthy server
/// rejecting these credentials.
#[derive(Debug)]
enum ReconnectOutcome {
    /// The server accepts connections again — re-attach now.
    /// Remote lanes carry the already-negotiated connection into the attach;
    /// UDS carries `None` because its cheap readiness probe is a raw socket.
    Connectable(Option<Box<Connection>>),
    /// The UDS socket file disappeared: the server shut down cleanly and
    /// is not restarting. Only reachable on UDS dials; remote transports
    /// have no socket file to observe.
    SocketGone,
    /// The deadline elapsed with every probe still failing.
    TimedOut,
    /// The host refused the credentials (ADR-0133), so no retry can succeed.
    /// Remote lanes only.
    Refused(AttachError),
}

enum ProbeAttempt {
    Connectable(Option<Box<Connection>>),
    SocketGone,
    Unavailable,
    /// This probe was refused, not merely unanswered: retrying cannot
    /// change the verdict, so the wait ends on it.
    Refused(AttachError),
}

/// Clear the "lost the server connection" / "server is back" pair before
/// the next attach enters the alt screen.
fn erase_reconnect_banner() {
    eprint!("\x1b[2A\x1b[J");
}

/// Ctrl-C during the cooked countdown. The line already says this is how
/// to give up; dying inside the next color probe is what leaves
/// `^[]10;rgb:...` and a `%` on the prompt.
#[allow(
    clippy::exit,
    reason = "SIGINT during the cooked reconnect window must exit now, matching the TUI signal path"
)]
fn give_up_reconnect() -> ! {
    eprint!("\r\x1b[K\x1b[A\x1b[K");
    std::process::exit(130);
}

async fn interrupt_reconnect() {
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
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
/// without a server; each shape names its cause and `phux doctor`.
/// `Connectable` maps to an empty report.
fn reconnect_failure_lines(outcome: &ReconnectOutcome, deadline: Duration) -> Vec<String> {
    match outcome {
        ReconnectOutcome::Connectable(_) => Vec::new(),
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
        ReconnectOutcome::Refused(err) => vec![
            format!("phux: the server refused the reconnect: {err}"),
            "  the credentials this attach dialed with are no longer accepted;".to_owned(),
            "  re-pair the host with `phux host add HOST` and attach again".to_owned(),
        ],
    }
}

/// Drive [`wait_until_connectable`] while painting a `\r`-overwritten
/// per-second countdown on stderr, erased before returning.
async fn wait_with_countdown(dial: &Dial, policy: ReconnectPolicy) -> ReconnectOutcome {
    let end = Instant::now() + policy.deadline;
    let mut probe = std::pin::pin!(wait_until_connectable(dial, policy));
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
            () = interrupt_reconnect() => give_up_reconnect(),
        }
    }
}

/// Flush the residual UTF-8 tail, backfill `duration`, and close the cast.
/// Idempotent: the clean-detach path finalizes before `process::exit`.
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
/// UDS short-circuits to [`ReconnectOutcome::SocketGone`] when the socket file is
/// gone (a clean shutdown unlinks it; an upgrade does not). Remote lanes return
/// the negotiated connection for the next attach to reuse, and a refusal ends
/// the ladder immediately (ADR-0133). The first attempt is immediate; `policy`
/// sets the deadline and cadence.
async fn wait_until_connectable(dial: &Dial, policy: ReconnectPolicy) -> ReconnectOutcome {
    let end = tokio::time::Instant::now() + policy.deadline;
    let mut backoff = policy.ladder.floor;
    loop {
        match tokio::time::timeout_at(end, probe_connectability(dial)).await {
            Ok(ProbeAttempt::Connectable(connection)) => {
                return ReconnectOutcome::Connectable(connection);
            }
            Ok(ProbeAttempt::SocketGone) => return ReconnectOutcome::SocketGone,
            // No amount of ladder changes a refused token, and the deadline
            // is the user's attach-wait window, not a grace period for
            // credentials: end here, with the reason they can act on.
            Ok(ProbeAttempt::Refused(err)) => return ReconnectOutcome::Refused(err),
            Ok(ProbeAttempt::Unavailable) => {}
            Err(_) => return ReconnectOutcome::TimedOut,
        }
        if tokio::time::timeout_at(end, tokio::time::sleep(backoff))
            .await
            .is_err()
        {
            return ReconnectOutcome::TimedOut;
        }
        backoff = policy.ladder.next(backoff);
    }
}

async fn probe_connectability(dial: &Dial) -> ProbeAttempt {
    match dial {
        Dial::Uds(path) => {
            if !path.exists() {
                return ProbeAttempt::SocketGone;
            }
            if tokio::net::UnixStream::connect(path).await.is_ok() {
                ProbeAttempt::Connectable(None)
            } else {
                ProbeAttempt::Unavailable
            }
        }
        // The returned connection is reused by the TUI, so its HELLO must
        // carry the same capabilities and client name as the initial attach.
        Dial::Quic(_) | Dial::Ws(_) => match phux_tui::attach::connect_for_attach(dial).await {
            Ok(conn) => ProbeAttempt::Connectable(Some(Box::new(conn))),
            // ADR-0133: the runtime owns which refusals no retry can
            // satisfy. A 401/403 on the upgrade or a refused QUIC preamble
            // ends the wait; everything else — an unanswered dial, a 503, a
            // half-open lane — may heal and walks the ladder.
            Err(err) if err.is_fatal_refusal() => ProbeAttempt::Refused(err),
            Err(_) => ProbeAttempt::Unavailable,
        },
    }
}

/// Result of a registered remote attach, including whether its saved route
/// failed early enough that ssh can repair it, and how.
pub(crate) struct RemoteAttachOutcome {
    pub(crate) code: ExitCode,
    pub(crate) repair: Repair,
}

/// Whether an ssh repair should follow a failed registered attach: only
/// failures establishing the saved transport authorize one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Repair {
    /// The attach got past the transport: nothing ssh can fix.
    None,
    /// Nobody answered at the saved route: the server is most likely
    /// stopped. Start it over ssh and dial again with the saved
    /// credentials; only re-pair if that still fails.
    Start,
    /// A host answered and refused the saved credentials, or the entry
    /// cannot be dialed as written: only a re-pair rewrites what is wrong.
    RePair,
}

impl RemoteAttachOutcome {
    const fn terminal(code: ExitCode) -> Self {
        Self {
            code,
            repair: Repair::None,
        }
    }

    const fn repairable(code: ExitCode) -> Self {
        Self {
            code,
            repair: Repair::RePair,
        }
    }
}

/// Attach through a registered `[[remote]]` entry (ADR-0055). `ssh://`
/// re-execs `ssh -t HOST phux attach`; the session still lives on the remote
/// server.
pub(crate) fn run_attach_remote(
    entry: &RemoteEntry,
    session: Option<String>,
    rec: Option<&RecordSpec>,
) -> ExitCode {
    run_attach_remote_outcome(entry, session, rec).code
}

/// [`run_attach_remote`] plus the early direct-route failure classification
/// the registered-host ladder uses to repair a cold host over ssh.
pub(crate) fn run_attach_remote_outcome(
    entry: &RemoteEntry,
    session: Option<String>,
    rec: Option<&RecordSpec>,
) -> RemoteAttachOutcome {
    let endpoint = match Endpoint::parse(&entry.endpoint) {
        Ok(endpoint) => endpoint,
        Err(err) => {
            eprintln!("phux: remote {:?}: {err}", entry.name);
            return RemoteAttachOutcome::repairable(ExitCode::FAILURE);
        }
    };
    let session = session.or_else(|| entry.session.clone());
    // ADR-0140: the sidebar draws this host's sessions live and every other
    // machine from the hosts provider; it needs to know which one this is.
    phux_tui::attach::hosts::set_attach_origin(phux_tui::attach::hosts::AttachOrigin::Remote(
        entry.name.clone(),
    ));

    let token = match remote::read_token(entry) {
        Ok(token) => token,
        Err(err) => {
            eprintln!("phux: remote {:?}: {err}", entry.name);
            return RemoteAttachOutcome::repairable(ExitCode::FAILURE);
        }
    };
    let identity = match entry.client_identity() {
        Ok(identity) => identity,
        Err(err) => {
            eprintln!("phux: {err}");
            return RemoteAttachOutcome::repairable(ExitCode::FAILURE);
        }
    };
    let credentials = RemoteCredentials {
        token,
        cert_fingerprint: entry.cert_fingerprint.clone(),
        identity,
    };

    match endpoint {
        Endpoint::Quic(target) => run_attach_quic_outcome(session, target, credentials, None, rec),
        Endpoint::Ws(url) => run_attach_ws_outcome(session, url, credentials, None, rec),
        // ADR-0120: bootstrap a direct QUIC attach over ssh first, and only
        // fall back to `exec`ing `ssh -t HOST phux attach` when that cannot
        // work. Only the direct path can carry a recording.
        // The bootstrapped listener is the host's own server, so it admits
        // the workload certificate `host add` enrolled there (ADR-0116).
        Endpoint::Ssh(host) => RemoteAttachOutcome::terminal(super::ssh_bootstrap::run(
            super::ssh_bootstrap::SshAttach {
                destination: host,
                session,
                remote_phux: "phux".to_owned(),
                udp_ports: None,
                identity: credentials.identity,
                rec,
            },
        )),
    }
}

/// Replace this process with `ssh -t HOST REMOTE_PHUX attach [SESSION]`, so
/// the terminal, signals, and exit code belong to ssh directly.
pub(crate) fn run_attach_over_ssh(
    host: &str,
    remote_phux: &str,
    session: Option<&str>,
) -> ExitCode {
    use std::os::unix::process::CommandExt as _;

    let program = std::env::var_os("PHUX_SSH").unwrap_or_else(|| "ssh".into());
    let mut command = std::process::Command::new(&program);
    command.arg("-t").arg(host).arg(remote_phux).arg("attach");
    if let Some(session) = session {
        command.arg(session);
    }

    // `exec` only returns on failure.
    let err = command.exec();
    eprintln!("phux: could not exec {}: {err}", program.to_string_lossy());
    ExitCode::FAILURE
}

/// `phux attach [NAME]` with no recording.
pub(crate) fn run_attach(session: Option<String>, socket: Option<PathBuf>) -> ExitCode {
    run_attach_rec(session, socket, None)
}

/// Run the attach loop on a current-thread runtime and map the result to an
/// exit code, auto-spawning `phux server` first if nothing accepts (see
/// [`ensure_server`]).
pub(crate) fn run_attach_rec(
    session: Option<String>,
    socket: Option<PathBuf>,
    rec: Option<&RecordSpec>,
) -> ExitCode {
    if let Err(code) = interactive_tty_preflight() {
        return code;
    }

    // A registered host name wins over a local session of the same name, unless
    // `--socket` says local; it goes through the same repair ladder as `--remote`.
    if socket.is_none()
        && let Some(name) = session.as_deref()
        && let Some(entry) = remote::find(name)
    {
        return super::remote_target::run_registered(name, entry, rec);
    }

    let socket_path = socket.unwrap_or_else(default_socket_path);
    // phux-iwuc: fail before auto-spawn with the sockaddr_un limit named,
    // instead of the 2s spawn timeout + a doomed connect.
    if let Err(code) = super::ensure_socket_path_fits(&socket_path) {
        return code;
    }
    // Resolve only the auto-spawn seed before moving `session` into the wire
    // target. With no explicit name this uses the configured template; after
    // startup the server owns that seed identity and resolves `Last`.
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
    let explicit_name = session.clone();
    let target = requested_attach_target(session);

    // Best-effort auto-spawn, pre-seeded with the session being attached to;
    // the attach below surfaces any connect error.
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

    // A no-name attach uses server-resolved `Last` first, with one lookup-only
    // name retry for older servers.
    let dial = Dial::uds(&socket_path);
    let predict_cfg = predictive_config_for(&dial);
    let result = match target {
        AttachTarget::Last => rt.block_on(attach_with_reconnect(
            &dial,
            AttachTarget::Last,
            predict_cfg,
            fallback_lookup_name(&default_name),
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
        // Attach never creates: a name the live server does not hold names
        // the verb that does, instead of only relaying the refusal.
        Err(AttachError::Refused(_))
            if explicit_name.as_deref().is_some_and(|name| {
                rt.block_on(phux_client::state::get_state(&socket_path))
                    .is_ok_and(|view| session_is_missing(view.snapshot(), name))
            }) =>
        {
            for line in missing_session_lines(explicit_name.as_deref().unwrap_or_default()) {
                eprintln!("{line}");
            }
            ExitCode::FAILURE
        }
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

/// Whether `snapshot` (a complete `GET_STATE`: session lists never
/// aggregate satellites) holds no session named `name`.
fn session_is_missing(snapshot: &phux_protocol::wire::info::SessionSnapshot, name: &str) -> bool {
    snapshot.sessions.iter().all(|session| session.name != name)
}

/// The refusal for `phux attach NAME` when the server has no `NAME`: attach
/// is lookup-only, so the remedy is the verb that creates one.
fn missing_session_lines(name: &str) -> Vec<String> {
    vec![
        format!("phux: no session named {name:?} on this server"),
        format!(
            "  create it with `phux new {}`, or run `phux ls` to list sessions",
            super::ssh_bootstrap::shell_quote(name)
        ),
    ]
}

/// Stderr hint for a non-loopback dial that got no answer: either an overlay
/// network is down, or a packet filter on the server host (on macOS the
/// application firewall silently drops an unrecognized, adhoc-signed binary)
/// swallows it. `phux doctor` on the server probes for the latter.
pub(crate) const OVERLAY_REACHABILITY_HINT: &str = "      The server did not answer or its name could not be resolved; credentials were never checked.\n      If the host lives on an overlay network (Tailscale/WireGuard), confirm the overlay is up on both ends.\n      If the overlay is up and ssh to the host works, suspect a firewall on the SERVER host instead:\n      run `phux doctor` there, which probes its own remote listener and names the blocker.";

/// The reachability hint applies only to [`AttachError::Unreachable`] on a
/// non-loopback target; a pin or auth failure means a host answered.
pub(crate) fn reachability_hint(err: &AttachError, loopback: bool) -> Option<&'static str> {
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

/// Resolve a `--quic` `HOST:PORT` to its first address plus the default TLS
/// server name. Resolution precedes the trust decision, which keys on the
/// resolved address.
fn resolve_quic_target(
    rt: &tokio::runtime::Runtime,
    target: &str,
) -> Result<(std::net::SocketAddr, String), DialRefusal> {
    let (host, port) = split_host_port(target).map_err(DialRefusal::Malformed)?;
    let bare_host = host.trim_matches(['[', ']']);
    let host_is_ip_literal = bare_host.parse::<std::net::IpAddr>().is_ok();

    let resolved = rt
        .block_on(tokio::net::lookup_host((bare_host, port)))
        .map(|mut addrs| addrs.next());
    let detail = match resolved {
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
    // An unresolvable name is the overlay-down shape (MagicDNS unreachable),
    // so it earns the same hint as an unanswered dial.
    Err(DialRefusal::Unresolved {
        target: target.to_owned(),
        detail,
        dns_name: !host_is_ip_literal,
    })
}

/// Attach over QUIC (ADR-0007) to `HOST:PORT`. TLS trust keys on the
/// resolved address: `--cert-fingerprint` pins the leaf; loopback without a pin
/// skips verification; routable without a pin is refused. With no session name
/// each reconnect starts with `Last`.
#[allow(
    clippy::needless_pass_by_value,
    reason = "clap hands over the owned HOST:PORT value; a &str signature would only push the borrow into lib.rs's dispatch"
)]
pub(crate) fn run_attach_quic(
    session: Option<String>,
    target: String,
    token: Option<String>,
    cert_fingerprint: Option<String>,
    server_name: Option<String>,
    identity: Option<phux_dial::TlsClientIdentity>,
    rec: Option<&RecordSpec>,
) -> ExitCode {
    let credentials = RemoteCredentials {
        token,
        cert_fingerprint,
        identity,
    };
    run_attach_quic_outcome(session, target, credentials, server_name, rec).code
}

/// What a remote dial authenticates with: the bearer token, the server pin,
/// and the workload client identity (`None` reads the environment).
struct RemoteCredentials {
    token: Option<String>,
    cert_fingerprint: Option<String>,
    identity: Option<phux_dial::TlsClientIdentity>,
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the owned connection inputs move into the Dial on the successful path"
)]
fn run_attach_quic_outcome(
    session: Option<String>,
    target: String,
    credentials: RemoteCredentials,
    server_name: Option<String>,
    rec: Option<&RecordSpec>,
) -> RemoteAttachOutcome {
    let RemoteCredentials {
        token,
        cert_fingerprint,
        identity,
    } = credentials;
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("failed to build runtime: {err}");
            return RemoteAttachOutcome::terminal(ExitCode::FAILURE);
        }
    };

    let DialPlan { dial, loopback } =
        match plan_quic_dial(&rt, &target, token, cert_fingerprint, server_name) {
            Ok(plan) => plan.with_identity(identity),
            Err(refusal) => {
                return RemoteAttachOutcome::repairable(refusal.report_for_attach());
            }
        };

    let predict_cfg = predictive_config_for(&dial);

    let default_name = resolved_default_session_name();
    let use_default = session.is_none();
    let attach_target = requested_attach_target(session);
    let default = use_default
        .then(|| fallback_lookup_name(&default_name))
        .flatten();

    let result = rt.block_on(attach_with_reconnect(
        &dial,
        attach_target,
        predict_cfg,
        default,
        rec,
    ));
    finalize_recording(rec);
    let repair = remote_repair(&result);
    let code = match result {
        Ok(end) => {
            report_attach_end(end);
            ExitCode::SUCCESS
        }
        // Already reported by the reconnect window (distinct SocketGone /
        // TimedOut lines naming `phux doctor`) — see `attach_with_reconnect`.
        Err(AttachError::Disconnected) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("phux: QUIC attach to {target} failed: {err}");
            if let Some(hint) = reachability_hint(&err, loopback) {
                eprintln!("{hint}");
            }
            ExitCode::FAILURE
        }
    };
    RemoteAttachOutcome { code, repair }
}

/// A remote dial ready to connect, plus whether it stays on this machine
/// (the failure hints key on it). Shared by `attach --quic/--ws` and the
/// headless verbs' `--remote`.
pub(crate) struct DialPlan {
    pub(crate) dial: Dial,
    pub(crate) loopback: bool,
}

impl DialPlan {
    /// Present `identity` on this dial's TLS handshake: a registry entry's
    /// enrolled workload certificate. `None` leaves the dial reading
    /// `PHUX_WORKLOAD_CERT` / `PHUX_WORKLOAD_KEY` as it always has.
    pub(crate) fn with_identity(mut self, identity: Option<phux_dial::TlsClientIdentity>) -> Self {
        match &mut self.dial {
            Dial::Quic(quic) => quic.identity = identity,
            Dial::Ws(ws) => ws.identity = identity,
            Dial::Uds(_) => {}
        }
        self
    }
}

/// Why a remote dial could not be planned; each caller words its own
/// remedy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DialRefusal {
    /// The endpoint text did not parse: a bad `HOST:PORT` or URL.
    Malformed(String),
    /// The host did not resolve. `dns_name` is false for an IP literal,
    /// which never touches DNS and so never earns the overlay hint.
    Unresolved {
        target: String,
        detail: String,
        dns_name: bool,
    },
    /// A routable QUIC endpoint with no certificate pin.
    UnpinnedQuic { target: String },
    /// A routable `wss://` endpoint with no certificate pin.
    UnpinnedWs { url: String },
    /// Plaintext `ws://` to a routable address.
    Plaintext { url: String },
    /// A routable `wss://` endpoint with no bearer token.
    NoToken { url: String },
    /// The bearer token is not well-formed hex.
    BadToken(String),
}

impl DialRefusal {
    /// The lines `phux attach --quic/--ws` prints for this refusal, the same
    /// wording the planners printed before they returned a typed error.
    fn attach_lines(&self) -> Vec<String> {
        match self {
            Self::Malformed(err) | Self::BadToken(err) => vec![format!("phux: {err}")],
            Self::Unresolved {
                target,
                detail,
                dns_name,
            } => {
                let mut lines = vec![format!("phux: QUIC attach to {target} failed: {detail}")];
                if *dns_name {
                    lines.push(OVERLAY_REACHABILITY_HINT.to_owned());
                }
                lines
            }
            Self::UnpinnedQuic { target } => vec![
                format!(
                    "phux: refusing to dial non-loopback QUIC server {target} without --cert-fingerprint."
                ),
                "      Run `phux pair` on the server host to print its certificate fingerprint,"
                    .to_owned(),
                format!("      then pass it: phux attach --quic {target} --cert-fingerprint <FP>"),
            ],
            Self::UnpinnedWs { url } => vec![
                format!(
                    "phux: refusing to dial non-loopback WebSocket server {url} without --cert-fingerprint."
                ),
                "      Run `phux pair` on the server host, then pass the printed fingerprint."
                    .to_owned(),
            ],
            Self::Plaintext { url } => vec![
                format!("phux: refusing plaintext WebSocket attach to non-loopback URL {url}."),
                "      Use wss:// plus `phux pair` credentials for remote devices.".to_owned(),
            ],
            Self::NoToken { url } => vec![
                format!("phux: refusing remote WebSocket attach to {url} without --token."),
                "      Run `phux pair` on the server host and pass the printed token once."
                    .to_owned(),
            ],
        }
    }

    /// Print the attach wording and return the failure exit code.
    pub(crate) fn report_for_attach(&self) -> ExitCode {
        for line in self.attach_lines() {
            eprintln!("{line}");
        }
        ExitCode::FAILURE
    }
}

/// Resolve `target` and build its QUIC dial; trust keys on the resolved
/// address.
pub(crate) fn plan_quic_dial(
    rt: &tokio::runtime::Runtime,
    target: &str,
    token: Option<String>,
    cert_fingerprint: Option<String>,
    server_name: Option<String>,
) -> Result<DialPlan, DialRefusal> {
    let (addr, default_server_name) = resolve_quic_target(rt, target)?;
    let loopback = addr.ip().is_loopback();
    let trust = quic_trust(target, cert_fingerprint, loopback)?;
    let token = parsed_quic_token(token)?;
    Ok(DialPlan {
        dial: Dial::Quic(QuicDial {
            addr,
            server_name: server_name.unwrap_or(default_server_name),
            token,
            trust,
            identity: None,
        }),
        loopback,
    })
}

/// Pin the certificate when a fingerprint was given, trust loopback's
/// self-signed dev cert, and refuse an unpinned routable dial.
fn quic_trust(
    target: &str,
    cert_fingerprint: Option<String>,
    loopback: bool,
) -> Result<CertTrust, DialRefusal> {
    if let Some(fingerprint) = cert_fingerprint {
        return Ok(CertTrust::Pinned(fingerprint));
    }
    if loopback {
        return Ok(CertTrust::SkipVerify);
    }
    Err(DialRefusal::UnpinnedQuic {
        target: target.to_owned(),
    })
}

/// Decode a hex bearer token for the QUIC preamble.
fn parsed_quic_token(token: Option<String>) -> Result<Option<Vec<u8>>, DialRefusal> {
    token
        .map(|token| attach::quic::parse_token_hex(&token))
        .transpose()
        .map_err(|err| DialRefusal::BadToken(err.to_string()))
}

/// Validate `url` and its credentials and build the WebSocket dial, or say
/// why it cannot be.
pub(crate) fn plan_ws_dial(
    url: String,
    token: Option<String>,
    cert_fingerprint: Option<String>,
    tls_server_name: Option<String>,
) -> Result<DialPlan, DialRefusal> {
    let target =
        attach::ws::WsTarget::parse(&url).map_err(|err| DialRefusal::Malformed(err.to_string()))?;
    require_ws_dial_credentials(&target, &url, token.as_deref(), cert_fingerprint.as_deref())?;
    let token = validated_ws_token(token)?;
    let trust = cert_fingerprint.map_or(CertTrust::SkipVerify, CertTrust::Pinned);
    Ok(DialPlan {
        dial: Dial::Ws(WsDial {
            url,
            token,
            trust,
            tls_server_name,
            identity: None,
        }),
        loopback: target.is_loopback(),
    })
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
    let credentials = RemoteCredentials {
        token,
        cert_fingerprint,
        identity: None,
    };
    run_attach_ws_outcome(session, url, credentials, tls_server_name, rec).code
}

fn run_attach_ws_outcome(
    session: Option<String>,
    url: String,
    credentials: RemoteCredentials,
    tls_server_name: Option<String>,
    rec: Option<&RecordSpec>,
) -> RemoteAttachOutcome {
    let RemoteCredentials {
        token,
        cert_fingerprint,
        identity,
    } = credentials;
    let DialPlan { dial, loopback } =
        match plan_ws_dial(url, token, cert_fingerprint, tls_server_name) {
            Ok(plan) => plan.with_identity(identity),
            Err(refusal) => {
                return RemoteAttachOutcome::repairable(refusal.report_for_attach());
            }
        };

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("failed to build runtime: {err}");
            return RemoteAttachOutcome::terminal(ExitCode::FAILURE);
        }
    };

    let predict_cfg = predictive_config_for(&dial);

    let default_name = resolved_default_session_name();
    let use_default = session.is_none();
    let target = requested_attach_target(session);
    let default = use_default
        .then(|| fallback_lookup_name(&default_name))
        .flatten();

    let result = rt.block_on(attach_with_reconnect(
        &dial,
        target,
        predict_cfg,
        default,
        rec,
    ));
    finalize_recording(rec);
    report_ws_attach_outcome(result, loopback)
}

/// Refuse a WebSocket dial that leaves the machine without a TLS pin or a
/// bearer token (or at all in plaintext), naming the flag that clears each.
fn require_ws_dial_credentials(
    target: &attach::ws::WsTarget,
    url: &str,
    token: Option<&str>,
    cert_fingerprint: Option<&str>,
) -> Result<(), DialRefusal> {
    if target.is_loopback() {
        return Ok(());
    }
    let url = url.to_owned();
    if !target.secure {
        return Err(DialRefusal::Plaintext { url });
    }
    if cert_fingerprint.is_none() {
        return Err(DialRefusal::UnpinnedWs { url });
    }
    if token.is_none() {
        return Err(DialRefusal::NoToken { url });
    }
    Ok(())
}

/// Check that a supplied bearer token is well-formed hex before it is dialed
/// with, and normalize its surrounding whitespace away.
fn validated_ws_token(token: Option<String>) -> Result<Option<String>, DialRefusal> {
    let Some(token) = token else {
        return Ok(None);
    };
    attach::quic::parse_token_hex(&token)
        .map(|_| Some(token.trim().to_owned()))
        .map_err(|err| DialRefusal::BadToken(err.to_string()))
}

/// The predictive-echo setting for one dial: the config's explicit value,
/// else on for a dial that leaves the machine. A config that fails to load
/// disables prediction: an unreadable file may well have said `false`.
pub(crate) fn predictive_config_for(dial: &Dial) -> PredictiveConfig {
    match config_loader::load() {
        Ok(cfg) => PredictiveConfig {
            enabled: cfg
                .experimental
                .predictive_echo_for(dial_leaves_the_machine(dial)),
        },
        Err(err) => {
            eprintln!(
                "phux: config load failed ({err}); predictive echo stays off \
                 (an unreadable config is not consent to turn it on)"
            );
            PredictiveConfig::disabled()
        }
    }
}

/// Whether this dial crosses a network. Keys on the resolved address for
/// QUIC and on the URL host for WebSocket (loopback is local whatever the
/// transport); an unparseable URL answers local, failing closed.
fn dial_leaves_the_machine(dial: &Dial) -> bool {
    match dial {
        Dial::Uds(_) => false,
        Dial::Quic(quic) => !quic.addr.ip().is_loopback(),
        Dial::Ws(ws) => {
            attach::ws::WsTarget::parse(&ws.url).is_ok_and(|target| !target.is_loopback())
        }
    }
}

/// Turn a finished WebSocket attach into its exit code and repair hint,
/// reporting the ending.
fn report_ws_attach_outcome(
    result: Result<AttachEnd, AttachError>,
    loopback: bool,
) -> RemoteAttachOutcome {
    let repair = remote_repair(&result);
    let code = match result {
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
    };
    RemoteAttachOutcome { code, repair }
}

/// Classify a failed remote attach for the ssh repair rung: an unanswered
/// dial wants the server started, a refused one wants a re-pair, and
/// anything past the transport wants nothing.
const fn remote_repair(result: &Result<AttachEnd, AttachError>) -> Repair {
    match result {
        Err(AttachError::Unreachable(_)) => Repair::Start,
        Err(AttachError::Connect(_)) => Repair::RePair,
        _ => Repair::None,
    }
}

#[cfg(test)]
mod tests {
    use phux_protocol::wire::frame::DetachReason;

    use super::*;

    /// The typed refusal renders the exact lines attach printed before the
    /// planners stopped printing: the flag-naming remedy for attach, and the
    /// overlay hint only after a DNS name failed to resolve.
    #[test]
    fn dial_refusals_keep_attach_wording() {
        let unpinned = DialRefusal::UnpinnedQuic {
            target: "mini:8788".to_owned(),
        }
        .attach_lines();
        assert_eq!(
            unpinned,
            vec![
                "phux: refusing to dial non-loopback QUIC server mini:8788 without --cert-fingerprint.",
                "      Run `phux pair` on the server host to print its certificate fingerprint,",
                "      then pass it: phux attach --quic mini:8788 --cert-fingerprint <FP>",
            ]
        );

        let unresolved = |dns_name| {
            DialRefusal::Unresolved {
                target: "mini:8788".to_owned(),
                detail: "name resolution failed: nope".to_owned(),
                dns_name,
            }
            .attach_lines()
        };
        assert_eq!(
            unresolved(true),
            vec![
                "phux: QUIC attach to mini:8788 failed: name resolution failed: nope".to_owned(),
                OVERLAY_REACHABILITY_HINT.to_owned(),
            ]
        );
        assert_eq!(
            unresolved(false).len(),
            1,
            "an IP literal never earns the hint"
        );

        assert_eq!(
            DialRefusal::NoToken {
                url: "wss://h:1".to_owned()
            }
            .attach_lines()[0],
            "phux: refusing remote WebSocket attach to wss://h:1 without --token."
        );
    }

    /// The gate is whether the dial actually crosses a network, not whether
    /// the transport could: a loopback QUIC or WebSocket dial has no round
    /// trip to hide, so it must not predict.
    #[test]
    fn predictive_echo_defaults_follow_whether_the_dial_leaves_the_machine() {
        let uds = Dial::uds(std::path::Path::new("/tmp/phux-test.sock"));
        let quic_loopback = quic_dial("127.0.0.1:8788");
        let quic_remote = quic_dial("203.0.113.7:8788");
        let ws_loopback = ws_dial("ws://127.0.0.1:8787");
        let ws_localhost = ws_dial("ws://localhost:8787");
        let ws_remote = ws_dial("wss://example.invalid:8787");

        assert!(
            !dial_leaves_the_machine(&uds),
            "UDS never leaves the machine"
        );
        assert!(
            !dial_leaves_the_machine(&quic_loopback),
            "a QUIC dial to loopback is a network transport with no network \
             between the ends; there is no latency to hide and only a \
             mispaint to collect"
        );
        assert!(
            !dial_leaves_the_machine(&ws_loopback),
            "the browser client talking ws://127.0.0.1 to a local server is a \
             normal way to run phux, not a corner case"
        );
        assert!(
            !dial_leaves_the_machine(&ws_localhost),
            "`localhost` is loopback by name as well as by address"
        );
        assert!(dial_leaves_the_machine(&quic_remote));
        assert!(dial_leaves_the_machine(&ws_remote));

        let unset = phux_config::ExperimentalCfg::default();
        for local in [&uds, &quic_loopback, &ws_loopback, &ws_localhost] {
            assert!(
                !unset.predictive_echo_for(dial_leaves_the_machine(local)),
                "a same-machine echo arrives in hundreds of microseconds; a \
                 prediction hides nothing and can only pay the flicker cases"
            );
        }
        for remote in [&quic_remote, &ws_remote] {
            assert!(
                unset.predictive_echo_for(dial_leaves_the_machine(remote)),
                "a dial that crosses a network pays a real round trip per key"
            );
        }

        let forced_on = phux_config::ExperimentalCfg {
            predictive_echo: Some(true),
        };
        assert!(
            forced_on.predictive_echo_for(dial_leaves_the_machine(&uds)),
            "an explicit opt-in reaches every transport"
        );

        let forced_off = phux_config::ExperimentalCfg {
            predictive_echo: Some(false),
        };
        assert!(
            !forced_off.predictive_echo_for(dial_leaves_the_machine(&quic_remote)),
            "an explicit opt-out must stick on every transport"
        );
    }

    fn quic_dial(addr: &str) -> Dial {
        Dial::Quic(QuicDial {
            addr: addr.parse().expect("addr"),
            server_name: "localhost".to_owned(),
            token: None,
            trust: CertTrust::SkipVerify,
            identity: None,
        })
    }

    fn ws_dial(url: &str) -> Dial {
        Dial::Ws(WsDial {
            url: url.to_owned(),
            token: None,
            trust: CertTrust::SkipVerify,
            tls_server_name: None,
            identity: None,
        })
    }

    #[test]
    fn unnamed_attach_and_fallback_preserve_lookup_only_authority() {
        assert_eq!(requested_attach_target(None), AttachTarget::Last);
        assert_eq!(
            default_lookup_target("configured"),
            AttachTarget::ByName("configured".to_owned()),
        );
        assert_eq!(
            requested_attach_target(Some("explicit".to_owned())),
            AttachTarget::ByName("explicit".to_owned()),
        );
    }

    /// `phux attach X` against a live server that has no `X` used to print
    /// only the server's refusal. Attach stays lookup-only (tmux's `attach -t`
    /// refuses the same way), so the miss names the verb that creates it.
    #[test]
    fn a_missing_session_names_the_create_and_list_verbs() {
        let session = |name: &str| {
            phux_protocol::wire::info::SessionInfo::new(phux_protocol::ids::SessionId::new(1), name)
        };
        let snapshot = phux_protocol::wire::info::SessionSnapshot::new(
            phux_protocol::ids::SessionId::new(1),
            phux_protocol::ids::WindowId::new(1),
            phux_protocol::ids::ResourceId::local(1),
        )
        .with_sessions(vec![session("main")]);
        assert!(session_is_missing(&snapshot, "work"));
        assert!(!session_is_missing(&snapshot, "main"));

        assert_eq!(
            missing_session_lines("work"),
            vec![
                "phux: no session named \"work\" on this server".to_owned(),
                "  create it with `phux new work`, or run `phux ls` to list sessions".to_owned(),
            ]
        );
        assert!(missing_session_lines("my work")[1].contains("`phux new 'my work'`"));
    }

    /// A detach says nothing; a last-pane death names the exit shape.
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
            Some("phux: session ended: the last pane was killed"),
        );
    }

    /// The reconnect probe is three-way: missing socket is `SocketGone`, a
    /// bound listener is `Connectable`, a path that never accepts is `TimedOut`.
    #[tokio::test]
    async fn reconnect_probe_distinguishes_gone_live_and_dead_sockets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("probe.sock");

        // No socket file: nothing to reconnect to — returns without waiting.
        let start = Instant::now();
        assert!(matches!(
            wait_until_connectable(&Dial::uds(&path), with_deadline(Duration::from_secs(5))).await,
            ReconnectOutcome::SocketGone
        ));
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "a missing socket should fail fast, not burn the deadline"
        );

        // A bound listener: connectable.
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        assert!(matches!(
            wait_until_connectable(&Dial::uds(&path), with_deadline(Duration::from_secs(2))).await,
            ReconnectOutcome::Connectable(None)
        ));
        drop(listener);

        // A path that exists but never accepts: the deadline elapses.
        std::fs::remove_file(&path).ok();
        std::fs::File::create(&path).expect("plug the socket path");
        assert!(matches!(
            wait_until_connectable(&Dial::uds(&path), with_deadline(Duration::from_millis(300)))
                .await,
            ReconnectOutcome::TimedOut
        ));
    }

    #[tokio::test]
    async fn remote_peer_that_accepts_transport_but_never_negotiates_cannot_overrun_deadline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind silent remote");
        let addr = listener.local_addr().expect("listener address");
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept reconnect probe");
            let _held_transport_open = stream;
            std::future::pending::<()>().await;
        });
        let dial = Dial::Ws(WsDial {
            url: format!("ws://{addr}"),
            token: None,
            trust: CertTrust::SkipVerify,
            tls_server_name: None,
            identity: None,
        });
        let policy = ReconnectPolicy {
            deadline: Duration::from_millis(300),
            ladder: Ladder::flat(Duration::from_millis(50)),
        };
        let start = Instant::now();
        assert!(matches!(
            wait_until_connectable(&dial, policy).await,
            ReconnectOutcome::TimedOut
        ));
        let elapsed = start.elapsed();
        assert!(
            elapsed >= policy.deadline,
            "deadline fired early: {elapsed:?}"
        );
        assert!(
            elapsed < policy.deadline + Duration::from_secs(2),
            "transport/protocol setup overran the absolute deadline: {elapsed:?}"
        );
        peer.abort();
        let _ = peer.await;
    }

    #[tokio::test]
    async fn reconnect_preserves_initial_tui_hello_and_reuses_the_connection() {
        use futures_util::{SinkExt, StreamExt};
        use phux_protocol::caps::{
            BootstrapCapabilities, ServerCapabilities, select_bootstrap_profile,
        };
        use phux_protocol::wire::frame::FrameKind;
        use tokio_tungstenite::tungstenite::Message;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dial = Dial::Ws(WsDial {
            url: format!("ws://{}", listener.local_addr().unwrap()),
            token: None,
            trust: CertTrust::SkipVerify,
            tls_server_name: None,
            identity: None,
        });
        let peer = tokio::spawn(async move {
            let mut offers = Vec::new();
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                let message = socket.next().await.unwrap().unwrap();
                let (hello, _) = FrameKind::decode(&message.into_data()).unwrap();
                let FrameKind::Hello { client_caps, .. } = &hello else {
                    panic!("first protocol frame must be HELLO");
                };
                let (selected_profile, bootstrap_limits) =
                    select_bootstrap_profile(client_caps, &BootstrapCapabilities::new()).unwrap();
                let mut bytes = bytes::BytesMut::new();
                FrameKind::HelloOk {
                    protocol_major: phux_protocol::PROTOCOL_VERSION.major,
                    protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
                    protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
                    server_caps: ServerCapabilities::new(),
                    server_id: Vec::new(),
                    selected_profile,
                    bootstrap_limits,
                }
                .encode(&mut bytes);
                socket.send(Message::Binary(bytes.freeze())).await.unwrap();
                offers.push(hello);
                let message = socket.next().await.unwrap().unwrap();
                assert!(
                    matches!(
                        FrameKind::decode(&message.into_data()).unwrap().0,
                        FrameKind::Ping { nonce: 42 }
                    ),
                    "connection was renegotiated instead of reused"
                );
                let mut bytes = bytes::BytesMut::new();
                FrameKind::Pong { nonce: 42 }.encode(&mut bytes);
                socket.send(Message::Binary(bytes.freeze())).await.unwrap();
            }
            offers
        });
        let mut initial = phux_tui::attach::connect_for_attach(&dial).await.unwrap();
        initial.send(&FrameKind::Ping { nonce: 42 }).await.unwrap();
        assert!(matches!(
            initial.recv().await.unwrap(),
            FrameKind::Pong { nonce: 42 }
        ));
        drop(initial);
        let ReconnectOutcome::Connectable(Some(mut resumed)) =
            wait_until_connectable(&dial, with_deadline(Duration::from_secs(2))).await
        else {
            panic!("remote reconnect did not retain its negotiated connection");
        };
        resumed.send(&FrameKind::Ping { nonce: 42 }).await.unwrap();
        assert!(matches!(
            resumed.recv().await.unwrap(),
            FrameKind::Pong { nonce: 42 }
        ));
        let offers = peer.await.unwrap();
        assert_eq!(
            offers[0], offers[1],
            "reconnect changed the TUI HELLO contract"
        );
    }

    /// A TCP peer that answers every WebSocket upgrade with `status`,
    /// counting how many it was asked for. Enough HTTP for tungstenite to
    /// read a rejection: the client never gets past the handshake, so the
    /// body and the connection lifetime after it do not matter.
    fn refusing_ws_peer(
        listener: tokio::net::TcpListener,
        status: &'static str,
    ) -> (
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);
        let peer = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut request = Vec::new();
                let mut byte = [0_u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut byte).await {
                        Ok(1) => request.push(byte[0]),
                        _ => break,
                    }
                }
                let _ = stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = stream.flush().await;
            }
        });
        (attempts, peer)
    }

    fn ws_dial_to(addr: std::net::SocketAddr) -> Dial {
        Dial::Ws(WsDial {
            url: format!("ws://{addr}"),
            token: None,
            trust: CertTrust::SkipVerify,
            tls_server_name: None,
            identity: None,
        })
    }

    /// ADR-0133: a refused reconnect ends on the first probe with the refusal.
    /// The deadline is far longer than the test would survive if the ladder were
    /// walked.
    #[tokio::test]
    async fn a_refused_reconnect_ends_on_the_first_probe_with_the_real_reason() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind refusing remote");
        let addr = listener.local_addr().expect("listener address");
        let (attempts, peer) = refusing_ws_peer(listener, "401 Unauthorized");
        let dial = ws_dial_to(addr);

        let start = Instant::now();
        let ReconnectOutcome::Refused(err) = wait_until_connectable(
            &dial,
            ReconnectPolicy {
                deadline: Duration::from_secs(60),
                ladder: Ladder::INTERACTIVE,
            },
        )
        .await
        else {
            panic!("a 401 must end the reconnect, not walk the ladder");
        };
        assert!(
            err.to_string().contains("401"),
            "the refusal must carry the status the server sent: {err}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "a refusal must not wait out the deadline: {:?}",
            start.elapsed()
        );
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a refused dial must not be retried"
        );

        let lines =
            reconnect_failure_lines(&ReconnectOutcome::Refused(err), Duration::from_secs(60));
        assert!(
            lines[0].contains("refused the reconnect") && lines[0].contains("401"),
            "the report names the refusal, not a timeout: {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("phux host add")),
            "a refused reconnect points at re-pairing: {lines:?}"
        );

        peer.abort();
        let _ = peer.await;
    }

    /// The other half of the same rule: a status that may heal is still
    /// worth the ladder, so the wait keeps probing until the deadline
    /// rather than treating any HTTP failure as terminal.
    #[tokio::test]
    async fn a_transient_refusal_still_walks_the_ladder() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind flaky remote");
        let addr = listener.local_addr().expect("listener address");
        let (attempts, peer) = refusing_ws_peer(listener, "503 Service Unavailable");
        let dial = ws_dial_to(addr);

        assert!(
            matches!(
                wait_until_connectable(
                    &dial,
                    ReconnectPolicy {
                        deadline: Duration::from_millis(400),
                        ladder: Ladder::flat(Duration::from_millis(50)),
                    },
                )
                .await,
                ReconnectOutcome::TimedOut
            ),
            "a 503 is not a refusal: the window must close on the deadline"
        );
        assert!(
            attempts.load(std::sync::atomic::Ordering::SeqCst) > 1,
            "a transient failure must be retried, not given up on"
        );

        peer.abort();
        let _ = peer.await;
    }

    /// The UDS policy with a test-length deadline; cadence untouched.
    fn with_deadline(deadline: Duration) -> ReconnectPolicy {
        ReconnectPolicy {
            deadline,
            ..UDS_RECONNECT
        }
    }

    /// The local lane's numbers are pinned: 100ms flat over 10s keeps the
    /// graceful-upgrade blink invisible.
    #[test]
    fn uds_reconnect_policy_is_unchanged_flat_100ms_over_10s() {
        let policy = reconnect_policy(&Dial::uds(std::path::Path::new("/tmp/phux-test.sock")));

        assert_eq!(policy.deadline, Duration::from_secs(10));
        assert_eq!(policy.ladder.floor, Duration::from_millis(100));
        assert_eq!(
            policy.ladder.ceiling,
            Duration::from_millis(100),
            "UDS must not back off — a graceful upgrade is over in <1s"
        );

        // Flat means flat, however many times it is applied.
        let mut backoff = policy.ladder.floor;
        for _ in 0..10 {
            backoff = policy.ladder.next(backoff);
            assert_eq!(backoff, Duration::from_millis(100));
        }
    }

    /// Both remote lanes get the patient, backing-off policy: each probe is a
    /// real handshake over a radio, and the thing that broke is usually the
    /// client's own network rather than the server.
    #[test]
    fn remote_lanes_get_backoff_and_a_longer_deadline() {
        let ws = reconnect_policy(&Dial::Ws(WsDial {
            url: "wss://example.ts.net:8788".to_owned(),
            token: None,
            trust: CertTrust::SkipVerify,
            tls_server_name: None,
            identity: None,
        }));
        let quic = reconnect_policy(&Dial::Quic(QuicDial {
            addr: "127.0.0.1:8788".parse().expect("addr"),
            server_name: "localhost".to_owned(),
            token: None,
            trust: CertTrust::SkipVerify,
            identity: None,
        }));

        assert_eq!(ws, quic, "the two remote lanes share one policy");
        assert!(
            ws.deadline > UDS_RECONNECT.deadline,
            "a wifi transition outlasts a re-exec: {:?}",
            ws.deadline
        );
        assert!(
            ws.ladder.ceiling > ws.ladder.floor,
            "remote probes must back off, not hammer TLS"
        );
        assert_eq!(
            ws.ladder,
            Ladder::INTERACTIVE,
            "the remote lanes walk the runtime's interactive ladder (ADR-0133)"
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

        let gone = reconnect_failure_lines(&ReconnectOutcome::SocketGone, deadline);
        assert_eq!(
            gone[0],
            "phux: the server shut down (its socket is gone) and is not restarting"
        );
        assert!(
            gone.iter().any(|l| l.contains("phux doctor")),
            "SocketGone must name phux doctor: {gone:?}"
        );

        let timed_out = reconnect_failure_lines(&ReconnectOutcome::TimedOut, deadline);
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
            reconnect_failure_lines(&ReconnectOutcome::Connectable(None), deadline).is_empty(),
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

    #[test]
    fn remote_repair_is_limited_to_early_transport_failures() {
        // A refused credential means a host answered: only a re-pair helps.
        assert_eq!(
            remote_repair(&Err(AttachError::Connect("bad credential".to_owned()))),
            Repair::RePair
        );
        // Nobody answered: the server is most likely stopped, so start it.
        assert_eq!(
            remote_repair(&Err(AttachError::Unreachable("no route".to_owned()))),
            Repair::Start
        );

        assert_eq!(
            remote_repair(&Err(AttachError::Refused("no such session".to_owned()))),
            Repair::None
        );
        assert_eq!(remote_repair(&Err(AttachError::Disconnected)), Repair::None);
        assert_eq!(
            remote_repair(&Ok(AttachEnd::Detached { reason: None })),
            Repair::None
        );
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
