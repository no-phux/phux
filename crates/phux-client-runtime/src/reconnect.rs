//! Reconnect policy: the backoff ladder and the refusals no retry can
//! satisfy.
//!
//! One implementation for every consumer that reconnects (ADR-0133). The
//! ladder and the fatal rule were phux-mobile's; the TUI and Cockpit each
//! carried their own before this crate existed.

use std::time::Duration;

use phux_dial::DialError;

/// The first reconnect delay, and the delay a connection that re-attaches
/// resets to.
pub const BACKOFF_FLOOR: Duration = Duration::from_millis(500);

/// The longest reconnect delay: doubling stops here.
pub const BACKOFF_CEILING: Duration = Duration::from_secs(8);

/// The next reconnect delay after a failed attempt: double, capped at
/// [`BACKOFF_CEILING`].
#[must_use]
pub fn next_backoff(current: Duration) -> Duration {
    (current * 2).min(BACKOFF_CEILING)
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
