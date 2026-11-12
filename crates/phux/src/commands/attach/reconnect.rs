//! The reconnect window an attach opens when its server vanishes: how long
//! each lane waits, the cooked-terminal countdown, the probes, and the
//! report when nothing comes back.

use std::time::{Duration, Instant};

use phux_client::attach::connection::Connection;
use phux_client::attach::{AttachError, Dial};
use phux_client_runtime::reconnect::Ladder;

/// How long a vanished server is given to come back and how hard to poll,
/// per dial lane (see [`reconnect_policy`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ReconnectPolicy {
    /// How long to keep trying before giving up and exiting.
    pub(super) deadline: Duration,
    /// The cadence between attempts (the first is immediate): the runtime's
    /// ladder for this lane (ADR-0133), so the arithmetic exists once.
    pub(super) ladder: Ladder,
}

/// The local lane: the ADR-0032 graceful-upgrade blink. The re-exec'd server
/// keeps the socket bound and returns in well under a second; ten seconds lets a
/// real crash report promptly.
pub(super) const UDS_RECONNECT: ReconnectPolicy = ReconnectPolicy {
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
pub(super) const fn reconnect_policy(dial: &Dial) -> ReconnectPolicy {
    match dial {
        Dial::Uds(_) => UDS_RECONNECT,
        _ => REMOTE_RECONNECT,
    }
}

/// The status-bar notice text a post-reconnect attach shows (phux-i0e8.2.3).
pub(super) const RECONNECT_NOTICE_TEXT: &str = "re-attached after server restart";

/// How the bounded reconnect probe ended. A gone socket is a clean shutdown,
/// one that never accepts is a crash or hang, and a refusal is a healthy server
/// rejecting these credentials.
#[derive(Debug)]
pub(super) enum ReconnectOutcome {
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
    /// ssh refused the host's key; carries the fix. ssh lanes only.
    HostKey(String),
}

enum ProbeAttempt {
    Connectable(Option<Box<Connection>>),
    SocketGone,
    Unavailable,
    /// This probe was refused, not merely unanswered: retrying cannot
    /// change the verdict, so the wait ends on it.
    Refused(AttachError),
    /// ssh refused the host's key, with the fix: final, like a refusal.
    HostKey(String),
}

/// Clear the "lost the server connection" / "server is back" pair before
/// the next attach enters the alt screen.
pub(super) fn erase_reconnect_banner() {
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
pub(super) fn reconnect_progress_line(remaining: Duration) -> String {
    let secs = remaining
        .saturating_add(Duration::from_millis(999))
        .as_secs();
    format!("phux: reconnecting… {secs}s left (Ctrl-C to give up)")
}

/// The cooked-terminal failure report for a reconnect window that closed
/// without a server; each shape names its cause and `phux doctor`.
/// `Connectable` maps to an empty report.
pub(super) fn reconnect_failure_lines(
    outcome: &ReconnectOutcome,
    deadline: Duration,
) -> Vec<String> {
    match outcome {
        ReconnectOutcome::Connectable(_) => Vec::new(),
        ReconnectOutcome::SocketGone => vec![
            "the server shut down (its socket is gone) and is not restarting".to_owned(),
            "start a new one with `phux` (attaches, auto-starting a server) or `phux server`"
                .to_owned(),
            "run `phux doctor` for a health check".to_owned(),
        ],
        ReconnectOutcome::TimedOut => vec![
            format!(
                "the server did not come back within {}s — it may have crashed",
                deadline.as_secs()
            ),
            format!(
                "server log: {}",
                phux_server::telemetry::server_log_path().display()
            ),
            "run `phux doctor` for a health check".to_owned(),
        ],
        ReconnectOutcome::Refused(err) => vec![
            format!("the server refused the reconnect: {err}"),
            "the credentials this attach dialed with are no longer accepted;".to_owned(),
            "re-pair the host with `phux host add HOST` and attach again".to_owned(),
        ],
        ReconnectOutcome::HostKey(remedy) => vec![
            "ssh refused the host's key, so phux cannot log in again".to_owned(),
            remedy.clone(),
        ],
    }
}

/// Drive [`wait_until_connectable`] while painting a `\r`-overwritten
/// per-second countdown on stderr, erased before returning.
pub(super) async fn wait_with_countdown(dial: &Dial, policy: ReconnectPolicy) -> ReconnectOutcome {
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

/// Wait until the server accepts again on `dial`, or give up.
///
/// UDS short-circuits to [`ReconnectOutcome::SocketGone`] when the socket file is
/// gone (a clean shutdown unlinks it; an upgrade does not). Remote lanes return
/// the negotiated connection for the next attach to reuse, and a refusal ends
/// the ladder immediately (ADR-0133). The first attempt is immediate; `policy`
/// sets the deadline and cadence.
pub(super) async fn wait_until_connectable(
    dial: &Dial,
    policy: ReconnectPolicy,
) -> ReconnectOutcome {
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
            Ok(ProbeAttempt::HostKey(remedy)) => return ReconnectOutcome::HostKey(remedy),
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
    // An ssh lane drops when phux's ssh master does, so the network took the
    // login with it: log in again first, on the cooked terminal, or a
    // password or 2FA host fails every `BatchMode` probe. Each probe tries,
    // so a network still down when the lane dropped is waited out. A live
    // master returns at once; a down network fails without a prompt.
    if let Dial::Ssh { destination, .. } = dial {
        crate::commands::enroll::login(destination);
    }
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
        _ => match phux_tui::attach::connect_for_attach(dial).await {
            Ok(conn) => ProbeAttempt::Connectable(Some(Box::new(conn))),
            // ADR-0133: the runtime owns which refusals no retry can
            // satisfy. A 401/403 on the upgrade or a refused QUIC preamble
            // ends the wait; everything else — an unanswered dial, a 503, a
            // half-open lane — may heal and walks the ladder.
            Err(err) if err.is_fatal_refusal() => ProbeAttempt::Refused(err),
            // A changed key does not heal by waiting, and saying "it may have
            // crashed" after a minute would hide the one thing to do.
            Err(err) => match dial {
                Dial::Ssh { destination, .. } => {
                    phux_client_runtime::connection::host_key_remedy(&err.to_string(), destination)
                        .map_or(ProbeAttempt::Unavailable, ProbeAttempt::HostKey)
                }
                _ => ProbeAttempt::Unavailable,
            },
        },
    }
}
