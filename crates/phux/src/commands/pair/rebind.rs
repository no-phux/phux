//! Waiting for a restarted server to bind its listeners again.

use std::path::Path;
use std::time::Instant;

use super::{
    LISTENER_SETTLE, ListenerQueryError, REBIND_POLL, REBIND_QUERY_TIMEOUT, REBIND_WAIT,
    query_listeners_with,
};

/// One poll of the restarting server's listeners.
#[derive(Clone, Copy)]
enum ListenerProbe {
    /// A listener is bound: done.
    Bound,
    /// The server answers but has no listener bound yet.
    Unbound,
    /// The server does not answer: it is restarting.
    Down,
}

/// How far a restart has got, as [`wait_for_rebind`] sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rebind {
    /// The old server may still be answering; no restart seen yet.
    AwaitingRestart,
    /// The server went away: the restart is under way.
    Down,
    /// The restarted server has answered, unbound, since this instant.
    UpUnbound(Instant),
}

impl Rebind {
    /// Advance on one probe at `now`; whether waiting can stop. An unbound
    /// answer counts only after a restart was seen, and only once the new
    /// server has been up for [`LISTENER_SETTLE`].
    fn settled_after(&mut self, probe: ListenerProbe, now: Instant) -> bool {
        match (probe, *self) {
            (ListenerProbe::Bound, _) => return true,
            (ListenerProbe::Down, _) => *self = Self::Down,
            (ListenerProbe::Unbound, Self::Down) => *self = Self::UpUnbound(now),
            // Not restarted yet, or already counting the settle time.
            (ListenerProbe::Unbound, Self::AwaitingRestart | Self::UpUnbound(_)) => {}
        }
        matches!(*self, Self::UpUnbound(since) if now.duration_since(since) >= LISTENER_SETTLE)
    }
}

/// Poll until a listener is bound, or until the restarted server has been
/// up long enough that overlay detection would have recorded one.
pub(super) fn wait_for_rebind(socket: &Path) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("could not build a runtime to wait for the server: {err}"))?;
    let deadline = Instant::now() + REBIND_WAIT;
    let mut rebind = Rebind::AwaitingRestart;
    while Instant::now() < deadline {
        let probe = match query_listeners_with(&runtime, socket, Some(REBIND_QUERY_TIMEOUT)) {
            Ok(_) => ListenerProbe::Bound,
            Err(ListenerQueryError::Unbound { .. }) => ListenerProbe::Unbound,
            Err(ListenerQueryError::Unavailable(_)) => ListenerProbe::Down,
        };
        if rebind.settled_after(probe, Instant::now()) {
            return Ok(());
        }
        std::thread::sleep(REBIND_POLL);
    }
    match query_listeners_with(&runtime, socket, Some(REBIND_QUERY_TIMEOUT)) {
        Ok(_) | Err(ListenerQueryError::Unbound { .. }) => Ok(()),
        Err(ListenerQueryError::Unavailable(message)) => Err(format!(
            "the server at {} did not come back after the restart: {message}",
            socket.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{LISTENER_SETTLE, ListenerProbe, Rebind};
    use std::time::{Duration, Instant};

    #[test]
    fn unbound_before_any_restart_keeps_waiting() {
        let now = Instant::now();
        let mut rebind = Rebind::AwaitingRestart;
        assert!(!rebind.settled_after(ListenerProbe::Unbound, now + LISTENER_SETTLE * 4));
        assert_eq!(rebind, Rebind::AwaitingRestart);
    }

    #[test]
    fn a_restarted_server_settles_after_staying_up_unbound() {
        let now = Instant::now();
        let mut rebind = Rebind::AwaitingRestart;
        assert!(!rebind.settled_after(ListenerProbe::Down, now));
        assert!(!rebind.settled_after(ListenerProbe::Unbound, now));
        assert!(!rebind.settled_after(ListenerProbe::Unbound, now + LISTENER_SETTLE / 2));
        assert!(rebind.settled_after(ListenerProbe::Unbound, now + LISTENER_SETTLE));
    }

    #[test]
    fn going_down_again_restarts_the_settle_clock() {
        let now = Instant::now();
        let mut rebind = Rebind::Down;
        assert!(!rebind.settled_after(ListenerProbe::Unbound, now));
        assert!(!rebind.settled_after(ListenerProbe::Down, now + LISTENER_SETTLE));
        let later = now + LISTENER_SETTLE + Duration::from_millis(1);
        assert!(!rebind.settled_after(ListenerProbe::Unbound, later));
        assert_eq!(rebind, Rebind::UpUnbound(later));
    }

    #[test]
    fn a_bound_listener_ends_the_wait_from_any_state() {
        let now = Instant::now();
        for mut rebind in [
            Rebind::AwaitingRestart,
            Rebind::Down,
            Rebind::UpUnbound(now),
        ] {
            assert!(rebind.settled_after(ListenerProbe::Bound, now));
        }
    }
}
