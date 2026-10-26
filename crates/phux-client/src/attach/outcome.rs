//! The attach exit vocabulary.
//!
//! The error every attach path funnels into, and the "how did it end"
//! explanation the CLI prints after teardown. A leaf module, so callers that
//! need only these types import nothing heavier.

use std::io;

use phux_client_runtime::reconnect::is_fatal_refusal_detail;
use phux_client_runtime::session::SessionError;
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

    /// A SPEC §5 framing violation, already answered with
    /// `ERROR { FRAME_TOO_LARGE }` as §5 requires.
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

    /// The server answered `HELLO` (or `ATTACH`) with
    /// `ERROR { VERSION_INCOMPATIBLE }`. Same sentence as [`Self::Refused`];
    /// the variant is how a caller tells a live old server from a policy
    /// refusal without reading the message text.
    #[error("server refused attach: {0}")]
    IncompatibleProtocol(String),

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

/// Total and lossless: every transport failure is classified once, by the
/// runtime session, and keeps its variant here.
impl From<SessionError> for AttachError {
    fn from(error: SessionError) -> Self {
        match error {
            SessionError::Io(error) => Self::Io(error),
            SessionError::Connect(detail) => Self::Connect(detail),
            SessionError::Unreachable(detail) => Self::Unreachable(detail),
            SessionError::Disconnected => Self::Disconnected,
            SessionError::Protocol(detail) => Self::Protocol(detail),
            SessionError::Framing(violation) => Self::Framing(violation),
            SessionError::Refused(message) => Self::Refused(message),
            SessionError::IncompatibleProtocol(message) => Self::IncompatibleProtocol(message),
        }
    }
}

/// Whether `err` is a live server that speaks another protocol major.
#[must_use]
pub const fn is_incompatible_protocol(err: &AttachError) -> bool {
    matches!(err, AttachError::IncompatibleProtocol(_))
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
    use phux_client_runtime::session::SessionError;

    use super::{AttachError, is_incompatible_protocol};

    /// The conversion keeps each variant, so the reconnect loop's
    /// `Disconnected` and the fatal-refusal rule mean what the runtime
    /// classified.
    #[test]
    fn session_errors_keep_their_variant_and_fatality() {
        assert!(matches!(
            AttachError::from(SessionError::Disconnected),
            AttachError::Disconnected
        ));
        assert!(matches!(
            AttachError::from(SessionError::Unreachable("no route".to_owned())),
            AttachError::Unreachable(_)
        ));
        let refused = SessionError::Connect("pairing token refused (unauthorized)".to_owned());
        assert!(refused.is_fatal_refusal());
        assert!(AttachError::from(refused).is_fatal_refusal());
        assert!(
            !AttachError::from(SessionError::Refused("no such session".to_owned()))
                .is_fatal_refusal()
        );
        let old = SessionError::IncompatibleProtocol("incompatible protocol".to_owned());
        assert!(matches!(
            AttachError::from(old),
            AttachError::IncompatibleProtocol(message) if message == "incompatible protocol"
        ));
        assert_eq!(
            AttachError::IncompatibleProtocol("no".to_owned()).to_string(),
            AttachError::Refused("no".to_owned()).to_string(),
        );
        assert!(is_incompatible_protocol(
            &AttachError::IncompatibleProtocol("no".to_owned())
        ));
        assert!(!is_incompatible_protocol(&AttachError::Refused(
            "no".to_owned()
        )));
    }
}
