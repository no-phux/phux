//! Reconnect policy: the backoff ladder and the refusals no retry can
//! satisfy.
//!
//! One implementation for every consumer that reconnects (ADR-0133). The
//! ladder and the fatal rule were phux-mobile's; the TUI, the agent verbs,
//! and Cockpit each carried their own before this crate existed. A consumer
//! picks a [`Ladder`] preset for its lane instead of forking the arithmetic:
//! the shape (double, hold at the ceiling, reset to the floor on progress) is
//! the same on every lane; only the two rungs differ.

use std::time::Duration;

use phux_dial::DialError;

/// A backoff ladder: the first reconnect delay and the delay doubling
/// stops at.
///
/// A consumer walks it by calling [`Ladder::next`] after each failed
/// attempt and resetting to [`Ladder::floor`] once a connection makes
/// progress. The presets are the ADR-0133 lanes; a consumer whose lane is
/// none of them constructs one and says why at the call site, so the
/// numbers still read as a policy rather than a magic sleep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ladder {
    /// The first reconnect delay, and the delay a connection that made
    /// progress resets to.
    pub floor: Duration,
    /// The longest reconnect delay: doubling stops here.
    pub ceiling: Duration,
}

impl Ladder {
    /// The interactive lane: a human attached over a network (the TUI's
    /// remote dials, the mobile bridge). Each probe is a real TLS handshake
    /// on a radio, and the thing that broke is usually the client's own
    /// network, so the ladder is patient: 500 ms doubling to 8 s.
    pub const INTERACTIVE: Self = Self {
        floor: Duration::from_millis(500),
        ceiling: Duration::from_secs(8),
    };

    /// The agent-verb lane: a headless verb (`phux resource wait`) that a
    /// caller is blocking on over the local socket. A reconnect here is a
    /// cheap Unix connect, and every millisecond of pause is latency the
    /// agent sees, so the ladder is fast: 50 ms doubling to 1 s. It still
    /// doubles so a gap that repeats on every connection waits out the
    /// caller's deadline instead of spinning.
    pub const AGENT_VERB: Self = Self {
        floor: Duration::from_millis(50),
        ceiling: Duration::from_secs(1),
    };

    /// The local-upgrade lane: the ADR-0032 graceful-upgrade blink on a
    /// Unix socket. The re-exec'd server keeps the socket bound and is back
    /// in well under a second, and a `connect` that is not accepted is a
    /// purely local failure, so a flat 100 ms poll re-attaches almost
    /// invisibly and there is nothing to be gentle about.
    pub const LOCAL_UPGRADE: Self = Self::flat(Duration::from_millis(100));

    /// A ladder that never grows: every delay is `delay`.
    #[must_use]
    pub const fn flat(delay: Duration) -> Self {
        Self {
            floor: delay,
            ceiling: delay,
        }
    }

    /// The next reconnect delay after one that waited `current`: double,
    /// capped at [`Self::ceiling`]. A flat ladder never grows.
    #[must_use]
    pub fn next(self, current: Duration) -> Duration {
        current.saturating_mul(2).min(self.ceiling)
    }
}

/// The first reconnect delay on the interactive lane, and the delay a
/// connection that re-attaches resets to.
pub const BACKOFF_FLOOR: Duration = Ladder::INTERACTIVE.floor;

/// The longest reconnect delay on the interactive lane: doubling stops here.
pub const BACKOFF_CEILING: Duration = Ladder::INTERACTIVE.ceiling;

/// The next reconnect delay on the interactive lane after a failed attempt:
/// double, capped at [`BACKOFF_CEILING`].
///
/// This is [`Ladder::INTERACTIVE`]'s [`Ladder::next`], kept as the
/// plain-function API for the consumers that walk only that lane.
#[must_use]
pub fn next_backoff(current: Duration) -> Duration {
    Ladder::INTERACTIVE.next(current)
}

/// Whether a refusal is one that retrying with the same credentials cannot
/// change.
///
/// Such a session fails now with the real reason instead of walking the
/// ladder past the caller's attach-wait window and reporting a generic
/// timeout.
///
/// Two shapes qualify. The QUIC preamble answered [`DialError::AuthRefused`].
/// The WebSocket upgrade was rejected with HTTP 401 or 403 by the ADR-0031
/// pairing gate; the status stays in the detail so a consumer's diagnostics
/// can key on it. Every other failure — a 503, a refused connect, a name
/// that did not resolve, a stall — may heal and is retried.
#[must_use]
pub fn is_fatal_refusal(error: &DialError) -> bool {
    match error {
        DialError::AuthRefused(_) => true,
        DialError::Connect(detail) => detail.contains("401") || detail.contains("403"),
        DialError::Io(_) | DialError::Unreachable(_) | DialError::Stalled(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_the_ceiling() {
        let mut delay = BACKOFF_FLOOR;
        let mut ladder = vec![delay];
        for _ in 0..6 {
            delay = next_backoff(delay);
            ladder.push(delay);
        }
        assert_eq!(
            ladder.iter().map(Duration::as_millis).collect::<Vec<_>>(),
            vec![500, 1000, 2000, 4000, 8000, 8000, 8000],
        );
    }

    fn schedule(ladder: Ladder, steps: usize) -> Vec<u128> {
        let mut delay = ladder.floor;
        let mut out = vec![delay.as_millis()];
        for _ in 0..steps {
            delay = ladder.next(delay);
            out.push(delay.as_millis());
        }
        out
    }

    /// The plain-function API is the interactive preset, so a consumer that
    /// uses either sees the same ladder.
    #[test]
    fn the_free_functions_are_the_interactive_preset() {
        assert_eq!(BACKOFF_FLOOR, Ladder::INTERACTIVE.floor);
        assert_eq!(BACKOFF_CEILING, Ladder::INTERACTIVE.ceiling);
        assert_eq!(
            next_backoff(Duration::from_secs(3)),
            Ladder::INTERACTIVE.next(Duration::from_secs(3))
        );
    }

    /// The agent-verb lane keeps `phux resource wait`'s 50 ms..1 s cadence
    /// (an agent is blocking on it) rather than the interactive one.
    #[test]
    fn the_agent_verb_ladder_is_fast_and_still_doubles() {
        assert_eq!(
            schedule(Ladder::AGENT_VERB, 6),
            vec![50, 100, 200, 400, 800, 1000, 1000]
        );
    }

    /// The local-upgrade lane is flat: a graceful upgrade is over in under
    /// a second and a Unix connect costs nothing, so it never backs off.
    #[test]
    fn the_local_upgrade_ladder_is_flat() {
        assert_eq!(schedule(Ladder::LOCAL_UPGRADE, 10), vec![100; 11]);
        assert_eq!(
            Ladder::flat(Duration::from_millis(7)).ceiling,
            Duration::from_millis(7)
        );
    }

    /// Doubling saturates instead of overflowing, so a runaway loop still
    /// holds at the ceiling.
    #[test]
    fn next_saturates_at_the_ceiling() {
        assert_eq!(Ladder::INTERACTIVE.next(Duration::MAX), BACKOFF_CEILING);
    }

    fn refused(status: u16, reason: &str) -> DialError {
        DialError::Connect(format!(
            "WebSocket handshake: HTTP error: {status} {reason}"
        ))
    }

    #[test]
    fn auth_rejections_are_fatal_and_everything_else_retries() {
        assert!(
            is_fatal_refusal(&refused(401, "Unauthorized")),
            "401 cannot heal"
        );
        assert!(
            is_fatal_refusal(&refused(403, "Forbidden")),
            "403 cannot heal"
        );
        assert!(
            is_fatal_refusal(&DialError::AuthRefused("unauthorized".to_owned())),
            "a QUIC token refusal is the same verdict as a 401"
        );
        // A gateway hiccup is worth the ladder.
        assert!(!is_fatal_refusal(&refused(503, "Service Unavailable")));
        assert!(!is_fatal_refusal(&DialError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "Connection refused (os error 61)",
        ))));
        assert!(!is_fatal_refusal(&DialError::Unreachable(
            "failed to lookup address information".to_owned()
        )));
        assert!(!is_fatal_refusal(&DialError::Stalled("no pong".to_owned())));
    }
}
