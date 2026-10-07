//! Reconnect policy for every consumer (ADR-0133).
//!
//! The backoff ladder and the refusals no retry can satisfy. Lanes share one
//! shape (double, hold at the ceiling, reset to the floor on progress) and
//! differ only in their [`Ladder`] rungs.

use std::time::Duration;

use phux_dial::DialError;

/// A backoff ladder: the first reconnect delay and the delay doubling
/// stops at. Walk it with [`Ladder::next`]; reset to [`Ladder::floor`] on
/// progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ladder {
    /// The first reconnect delay, and the delay a connection that made
    /// progress resets to.
    pub floor: Duration,
    /// The longest reconnect delay: doubling stops here.
    pub ceiling: Duration,
}

impl Ladder {
    /// The interactive lane (a human over a network, where the client's own
    /// radio usually broke): 500 ms doubling to 8 s.
    pub const INTERACTIVE: Self = Self {
        floor: Duration::from_millis(500),
        ceiling: Duration::from_secs(8),
    };

    /// The agent-verb lane (a caller blocking on a cheap local reconnect):
    /// 50 ms doubling to 1 s.
    pub const AGENT_VERB: Self = Self {
        floor: Duration::from_millis(50),
        ceiling: Duration::from_secs(1),
    };

    /// The local-upgrade lane (the ADR-0032 re-exec blink on a still-bound
    /// Unix socket): a flat 100 ms poll.
    pub const LOCAL_UPGRADE: Self = Self::flat(Duration::from_millis(100));

    /// A ladder that never grows: every delay is `delay`.
    #[must_use]
    pub const fn flat(delay: Duration) -> Self {
        Self {
            floor: delay,
            ceiling: delay,
        }
    }

    /// The next delay after one that waited `current`: double, clamped to
    /// `[floor, ceiling]` (so a zero seed starts at the floor).
    #[must_use]
    pub fn next(self, current: Duration) -> Duration {
        current.saturating_mul(2).clamp(self.floor, self.ceiling)
    }
}

/// Whether retrying with the same credentials cannot change this refusal:
/// a QUIC [`DialError::AuthRefused`] or an ADR-0031 401/403 on the
/// WebSocket upgrade. Everything else may heal and is retried.
#[must_use]
pub fn is_fatal_refusal(error: &DialError) -> bool {
    match error {
        DialError::AuthRefused(_) | DialError::AuthorityChanged(_) => true,
        DialError::Connect(detail) => is_fatal_refusal_detail(detail),
        DialError::Io(_) | DialError::Unreachable(_) | DialError::Stalled(_) => false,
    }
}

/// The phrase a consumer uses when it flattens [`DialError::AuthRefused`]
/// into its own vocabulary; [`is_fatal_refusal_detail`] reads it back.
pub const TOKEN_REFUSED: &str = "pairing token refused";

/// The phrase every rendering of [`DialError::AuthorityChanged`] carries;
/// [`is_fatal_refusal_detail`] reads it back.
pub const AUTHORITY_CHANGED: &str = "certificate authority changed";

/// [`is_fatal_refusal`] for a consumer holding only the rendered detail of
/// a connect failure.
#[must_use]
pub fn is_fatal_refusal_detail(detail: &str) -> bool {
    detail.contains("401")
        || detail.contains("403")
        || detail.contains(TOKEN_REFUSED)
        || detail.contains(AUTHORITY_CHANGED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_the_ceiling() {
        assert_eq!(
            schedule(Ladder::INTERACTIVE, 6),
            vec![500, 1000, 2000, 4000, 8000, 8000, 8000]
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
        assert_eq!(
            Ladder::INTERACTIVE.next(Duration::MAX),
            Ladder::INTERACTIVE.ceiling
        );
    }

    /// A seed below the floor (usually `Duration::ZERO`) lands on the floor.
    #[test]
    fn next_never_drops_below_the_floor() {
        assert_eq!(
            Ladder::INTERACTIVE.next(Duration::ZERO),
            Ladder::INTERACTIVE.floor
        );
        assert_eq!(
            Ladder::AGENT_VERB.next(Duration::ZERO),
            Ladder::AGENT_VERB.floor
        );
        assert_eq!(
            Ladder::LOCAL_UPGRADE.next(Duration::ZERO),
            Ladder::LOCAL_UPGRADE.floor
        );
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
        let changed = DialError::AuthorityChanged(phux_dial::AuthorityChange {
            pinned: format!("sha256:{}", "a".repeat(64)),
            presented: Some(format!("sha256:{}", "b".repeat(64))),
        });
        assert!(
            is_fatal_refusal(&changed),
            "a changed authority never heals"
        );
        assert!(
            is_fatal_refusal_detail(&crate::dial::dial_message("mini", &changed)),
            "the flattened refusal stays fatal"
        );
    }

    /// The rendered-detail form is the same verdict as the typed one, so a
    /// consumer that only kept the text of a connect failure — the attach
    /// loop, whose probe returns the attach vocabulary rather than a
    /// `DialError` — classifies it identically.
    #[test]
    fn the_detail_rule_matches_the_typed_rule() {
        for error in [
            refused(401, "Unauthorized"),
            refused(403, "Forbidden"),
            refused(503, "Service Unavailable"),
            DialError::Connect("server certificate fingerprint mismatch".to_owned()),
        ] {
            let DialError::Connect(detail) = &error else {
                unreachable!("every case above is a connect failure");
            };
            assert_eq!(
                is_fatal_refusal(&error),
                is_fatal_refusal_detail(detail),
                "the two spellings disagreed on {detail:?}"
            );
        }
    }

    /// A refused QUIC preamble survives being flattened into a consumer's
    /// connect vocabulary: the wording carries [`TOKEN_REFUSED`], so the
    /// detail rule still calls it fatal. This is the assertion that keeps
    /// `phux-client`'s `From<DialError>` wording and this rule together.
    #[test]
    fn a_flattened_token_refusal_stays_fatal() {
        assert!(is_fatal_refusal_detail(&format!(
            "{TOKEN_REFUSED} (unauthorized)"
        )));
        assert!(!is_fatal_refusal_detail(
            "did not answer (connection refused)"
        ));
    }
}
