//! The attach exit vocabulary.
//!
//! The error every attach path funnels into, and the "how did it end"
//! explanation the CLI prints after teardown. A leaf module, so callers that
//! need only these types import nothing heavier.

use std::io;

use phux_client_runtime::reconnect::{TOKEN_REFUSED, is_fatal_refusal_detail};
use phux_protocol::wire::frame::DetachReason;
use phux_protocol::wire::framing::FramingError;

/// Errors an attach or control-plane connection can surface. Distinct from
/// the server's internal registry `AttachError`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AttachError {
    /// Local I/O error: socket connect/read/write, stdin/stdout, or ioctl.
    #[error("attach loop io error: {0}")]
    Io(#[source] io::Error),

    /// A host answered but the remote transport could not be established
    /// (TLS pin mismatch, refused or oversized auth preamble).
    #[error("transport connect error: {0}")]
    Connect(String),

    /// The remote host did not answer the dial (refused, no route, timeout),
    /// so the CLI hints at reachability rather than credentials.
    #[error("transport connect error: {0}")]
    Unreachable(String),

    /// The server closed the connection without sending `DETACHED`.
    #[error("connection closed by server before DETACHED")]
    Disconnected,

    /// An undecodable frame, or a valid one unexpected at this point.
    #[error("protocol error: {0}")]
    Protocol(String),

    /// A SPEC §5 framing violation. Kept typed so
    /// [`super::connection::Connection::recv`] can answer it with
    /// `ERROR { FRAME_TOO_LARGE }` before closing, as §5 requires.
    #[error("protocol error: server sent a malformed frame: {0}")]
    Framing(#[from] FramingError),

    /// Could not put the outer terminal into the expected state.
    #[error("terminal control error: {0}")]
    Terminal(String),

    /// Stdin is not a terminal; attach needs one for raw mode.
    #[error("stdin is not a terminal; attach requires an interactive TTY")]
    NotATty,

    /// A libghostty operation failed on the client's local Terminal.
    #[error("libghostty: {0}")]
    Ghostty(#[from] libghostty_vt::Error),

    /// The server answered with a structured `ERROR` instead of the reply.
    #[error("server refused attach: {0}")]
    Refused(String),

    /// `GET_SCREEN` asked for a rendered capture and the `Ok` reply carried
    /// none: an older server ignores the `format` byte, or the server's own
    /// render failed (see `snapshot::get_screen_scrollback_format`).
    #[error("{0}")]
    FormatUnsupported(String),
}

impl From<io::Error> for AttachError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<phux_dial::DialError> for AttachError {
    fn from(value: phux_dial::DialError) -> Self {
        match value {
            phux_dial::DialError::Io(err) => Self::Io(err),
            phux_dial::DialError::Connect(msg) => Self::Connect(msg),
            // The host refused the token: re-pair, not an overlay loss.
            phux_dial::DialError::AuthRefused(msg) => {
                Self::Connect(format!("{TOKEN_REFUSED} ({msg})"))
            }
            phux_dial::DialError::Unreachable(msg) => Self::Unreachable(msg),
            // A stalled lane is a disconnection; mapping it here routes a
            // half-open `wss://` socket into the bounded reconnect.
            phux_dial::DialError::Stalled(msg) => {
                tracing::info!(reason = %msg, "WebSocket lane stalled; treating it as a disconnect");
                Self::Disconnected
            }
        }
    }
}

impl AttachError {
    /// Whether this is a transport refusal no retry with the same credentials
    /// can change (a refused pairing token, ADR-0031). Applies the
    /// `phux-client-runtime::reconnect` rule to the flattened detail.
    /// [`Self::Refused`] is the server's `ATTACH` policy, not the transport,
    /// and stays retryable.
    #[must_use]
    pub fn is_fatal_refusal(&self) -> bool {
        match self {
            Self::Connect(detail) => is_fatal_refusal_detail(detail),
            _ => false,
        }
    }
}

/// How a successful attach loop ended: an explanation, not an error, so the
/// CLI can tell "you detached" from "your last pane died".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachEnd {
    /// The server sent `DETACHED`, or the client tore its attach down.
    Detached {
        /// The `DETACHED` reason, or `None` when none was stated or known.
        /// `None` and `Requested` are the quiet, expected endings.
        reason: Option<DetachReason>,
    },
    /// The last pane's process exited, leaving nothing to attach to.
    LastPaneClosed {
        /// The pane's exit code, or `None` for signal kills / unknown causes.
        exit_status: Option<i32>,
    },
}

impl AttachEnd {
    /// One-line explanation for the terminal after teardown, or `None` when
    /// the ending needs no words (a detach the user asked for).
    #[must_use]
    pub fn explanation(self) -> Option<String> {
        match self {
            Self::Detached { reason } => match reason {
                None | Some(DetachReason::Requested) => None,
                Some(reason) => Some(format!("phux: detached: {}", reason.describe())),
            },
            Self::LastPaneClosed {
                exit_status: Some(code),
            } => Some(format!("phux: session ended: the last pane exited {code}")),
            // No exit code: a signal, `kill-pane`, or an unknown cause.
            Self::LastPaneClosed { exit_status: None } => {
                Some("phux: session ended: the last pane was killed".to_owned())
            }
        }
    }
}

/// Human phrase for a `RESOURCE_CLOSED` exit status, shared by every surface
/// that reports a pane exit.
#[must_use]
pub fn describe_exit(exit_status: Option<i32>) -> String {
    exit_status.map_or_else(
        || "killed (signal or unknown)".to_owned(),
        |code| format!("exited {code}"),
    )
}

#[cfg(test)]
mod tests {
    use phux_dial::DialError;

    use super::AttachError;

    /// The reconnect loop retries on `Disconnected` and nothing else, so a
    /// stalled WebSocket lane must map there, and faults worth reporting
    /// (pin mismatch, unreachable host) must not.
    #[test]
    fn a_stalled_lane_maps_to_disconnected_and_nothing_else_does() {
        let err = |e: DialError| AttachError::from(e);
        assert!(matches!(
            err(DialError::Stalled("no pong".to_owned())),
            AttachError::Disconnected
        ));
        assert!(matches!(
            err(DialError::Unreachable("no route".to_owned())),
            AttachError::Unreachable(_)
        ));
        assert!(matches!(
            err(DialError::Connect("pin mismatch".to_owned())),
            AttachError::Connect(_)
        ));
        assert!(matches!(
            err(DialError::AuthRefused("unauthorized".to_owned())),
            AttachError::Connect(msg) if msg.contains("pairing token refused")
        ));
        assert!(matches!(
            err(DialError::Io(std::io::ErrorKind::BrokenPipe.into())),
            AttachError::Io(_)
        ));
    }

    /// Refusals no retry can satisfy stay fatal after flattening; everything
    /// the reconnect ladder could heal stays retryable.
    #[test]
    fn only_the_refusals_no_retry_can_satisfy_are_fatal() {
        let http =
            |status: &str| DialError::Connect(format!("WebSocket handshake: HTTP error: {status}"));
        let cases = [
            (DialError::AuthRefused("unauthorized".to_owned()), true),
            (http("401 Unauthorized"), true),
            (http("403 Forbidden"), true),
            (http("503 Service Unavailable"), false),
            (DialError::Unreachable("no route".to_owned()), false),
            (DialError::Stalled("no pong".to_owned()), false),
            (
                DialError::Io(std::io::ErrorKind::ConnectionRefused.into()),
                false,
            ),
        ];
        for (error, fatal) in cases {
            let rendered = error.to_string();
            assert_eq!(
                AttachError::from(error).is_fatal_refusal(),
                fatal,
                "{rendered}"
            );
        }
        assert!(!AttachError::Refused("no such session".to_owned()).is_fatal_refusal());
    }
}
