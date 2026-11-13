//! Retry a remote bind that failed for a reason that clears by itself: a
//! server started at boot before the network assigned its listen address, or
//! while a port is still briefly held.

use phux_protocol::wire::RemoteListenerTransport;
use std::io::{self, ErrorKind};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const FIRST_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(30);
/// A port someone else holds is retried only this long: a process on its way
/// out frees it quickly; one that is staying never will.
const IN_USE_WINDOW: Duration = Duration::from_secs(60);

/// Whether a bind error may clear without operator action.
pub(super) fn is_transient(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        ErrorKind::AddrNotAvailable | ErrorKind::AddrInUse
    )
}

/// Bind and drop a throwaway socket of the kind `transport` listens on.
pub(super) fn probe_bind(transport: RemoteListenerTransport, addr: SocketAddr) -> io::Result<()> {
    match transport {
        RemoteListenerTransport::Wss => std::net::TcpListener::bind(addr).map(drop),
        _ => std::net::UdpSocket::bind(addr).map(drop),
    }
}

/// How [`retry_until_bindable`] ended.
#[derive(Debug)]
pub(super) enum Retried {
    /// The address binds now.
    Bindable,
    /// The error turned permanent, or the port stayed in use too long.
    GaveUp(io::Error),
    /// The server is shutting down.
    Cancelled,
}

/// Probe with capped exponential backoff until the address binds, the error
/// stops being transient (or the port stays in use past [`IN_USE_WINDOW`]),
/// or `token` fires. `on_change` runs once per change of error kind, never
/// per attempt, so a box waiting hours for its address logs one line.
pub(super) async fn retry_until_bindable(
    first: ErrorKind,
    mut probe: impl FnMut() -> io::Result<()>,
    mut on_change: impl FnMut(&io::Error),
    token: &CancellationToken,
) -> Retried {
    let mut last = first;
    let mut in_use_since = (first == ErrorKind::AddrInUse).then(Instant::now);
    let mut delay = FIRST_DELAY;
    loop {
        tokio::select! {
            () = token.cancelled() => return Retried::Cancelled,
            () = tokio::time::sleep(delay) => {}
        }
        delay = (delay * 2).min(MAX_DELAY);
        let err = match probe() {
            Ok(()) => return Retried::Bindable,
            Err(err) => err,
        };
        if !is_transient(&err) {
            return Retried::GaveUp(err);
        }
        if err.kind() == ErrorKind::AddrInUse {
            if in_use_since.get_or_insert_with(Instant::now).elapsed() >= IN_USE_WINDOW {
                return Retried::GaveUp(err);
            }
        } else {
            in_use_since = None;
        }
        if err.kind() != last {
            last = err.kind();
            on_change(&err);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(kind: ErrorKind) -> io::Error {
        io::Error::from(kind)
    }

    #[tokio::test(start_paused = true)]
    async fn a_bind_that_fails_then_succeeds_reports_bindable_and_logs_once() {
        let mut attempts = 0;
        let mut changes = Vec::new();
        let outcome = retry_until_bindable(
            ErrorKind::AddrNotAvailable,
            || {
                attempts += 1;
                match attempts {
                    1..=3 => Err(err(ErrorKind::AddrNotAvailable)),
                    4 => Err(err(ErrorKind::AddrInUse)),
                    _ => Ok(()),
                }
            },
            |e| changes.push(e.kind()),
            &CancellationToken::new(),
        )
        .await;
        assert!(matches!(outcome, Retried::Bindable), "{outcome:?}");
        assert_eq!(attempts, 5);
        // Three more "not available" are silent; only the switch to
        // "in use" is a state change.
        assert_eq!(changes, vec![ErrorKind::AddrInUse]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_missing_address_is_retried_indefinitely_with_capped_backoff() {
        let start = Instant::now();
        let mut attempts = 0_u32;
        let outcome = retry_until_bindable(
            ErrorKind::AddrNotAvailable,
            || {
                attempts += 1;
                if attempts < 200 {
                    Err(err(ErrorKind::AddrNotAvailable))
                } else {
                    Ok(())
                }
            },
            |_| panic!("no state change"),
            &CancellationToken::new(),
        )
        .await;
        assert!(matches!(outcome, Retried::Bindable));
        // Capped at 30s: 200 attempts take under two hours, not 2^200 s.
        assert!(start.elapsed() < Duration::from_secs(200 * 30));
    }

    #[tokio::test(start_paused = true)]
    async fn a_port_held_past_the_window_or_a_permanent_error_gives_up() {
        let token = CancellationToken::new();
        let held = retry_until_bindable(
            ErrorKind::AddrInUse,
            || Err(err(ErrorKind::AddrInUse)),
            |_| {},
            &token,
        )
        .await;
        assert!(matches!(held, Retried::GaveUp(ref e) if e.kind() == ErrorKind::AddrInUse));
        let denied = retry_until_bindable(
            ErrorKind::AddrNotAvailable,
            || Err(err(ErrorKind::PermissionDenied)),
            |_| {},
            &token,
        )
        .await;
        assert!(
            matches!(denied, Retried::GaveUp(ref e) if e.kind() == ErrorKind::PermissionDenied)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_stops_the_retry() {
        let token = CancellationToken::new();
        token.cancel();
        let outcome = retry_until_bindable(
            ErrorKind::AddrNotAvailable,
            || panic!("no probe after shutdown"),
            |_| {},
            &token,
        )
        .await;
        assert!(matches!(outcome, Retried::Cancelled));
    }

    #[test]
    fn only_address_errors_are_transient() {
        assert!(is_transient(&err(ErrorKind::AddrNotAvailable)));
        assert!(is_transient(&err(ErrorKind::AddrInUse)));
        assert!(!is_transient(&err(ErrorKind::PermissionDenied)));
    }
}
