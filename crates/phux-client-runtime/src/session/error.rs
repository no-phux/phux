//! What a one-shot session can fail with.

use std::io;

use phux_dial::DialError;
use phux_protocol::wire::framing::FramingError;

use crate::connection::DialFailure;
use crate::connection::io::LinkError;
use crate::reconnect::{TOKEN_REFUSED, is_fatal_refusal, is_fatal_refusal_detail};

/// Why a [`super::Session`] operation failed.
///
/// One variant per way a caller reacts: retry on [`Self::Disconnected`],
/// hint at reachability on [`Self::Unreachable`], re-pair on a
/// [`Self::is_fatal_refusal`] connect, answer [`Self::Framing`] before
/// closing. `phux-client`'s `AttachError` mirrors each variant by name, so
/// its conversion is total and loses nothing.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Socket connect, read, or write failed; the [`io::ErrorKind`] is kept,
    /// since a gone peer (`BrokenPipe`, `ConnectionReset`) and a missing
    /// server (`NotFound`, `ConnectionRefused`) are classified on it.
    #[error("io error: {0}")]
    Io(#[source] io::Error),

    /// A host answered but the remote transport could not be established
    /// (TLS pin mismatch, refused token, refused dial plan).
    #[error("transport connect error: {0}")]
    Connect(String),

    /// The remote host did not answer the dial (refused, no route, timeout).
    #[error("transport connect error: {0}")]
    Unreachable(String),

    /// The server closed the connection, or the link stopped carrying
    /// traffic; a caller may reconnect.
    #[error("connection closed by server")]
    Disconnected,

    /// An undecodable frame, or a valid one unexpected at this point.
    #[error("protocol error: {0}")]
    Protocol(String),

    /// A SPEC §5 framing violation, already answered with
    /// `ERROR { FRAME_TOO_LARGE }` as §5 requires.
    #[error("protocol error: server sent a malformed frame: {0}")]
    Framing(#[from] FramingError),

    /// The server answered `HELLO` with an `ERROR`.
    #[error("server refused: {0}")]
    Refused(String),
}

impl SessionError {
    /// Whether this is a transport refusal no retry with the same
    /// credentials can change (a refused pairing token, ADR-0031; a changed
    /// certificate authority, ADR-0153). [`Self::Refused`] is the server's
    /// policy, not the transport, and stays retryable.
    #[must_use]
    pub fn is_fatal_refusal(&self) -> bool {
        match self {
            Self::Connect(detail) => is_fatal_refusal_detail(detail),
            _ => false,
        }
    }
}

impl From<io::Error> for SessionError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<DialError> for SessionError {
    fn from(error: DialError) -> Self {
        match error {
            DialError::Io(error) => Self::Io(error),
            DialError::Connect(detail) => Self::Connect(detail),
            // The host refused the token: re-pair, not an overlay loss.
            DialError::AuthRefused(detail) => Self::Connect(format!("{TOKEN_REFUSED} ({detail})")),
            DialError::Unreachable(detail) => Self::Unreachable(detail),
            // Carries the AUTHORITY_CHANGED phrase, so it stays fatal.
            DialError::AuthorityChanged(change) => Self::Connect(change.to_string()),
            // A stalled lane is a disconnection, so a half-open `wss://`
            // socket routes into a caller's bounded reconnect.
            DialError::Stalled(detail) => {
                tracing::info!(reason = %detail, "WebSocket lane stalled; treating it as a disconnect");
                Self::Disconnected
            }
        }
    }
}

impl From<DialFailure> for SessionError {
    fn from(failure: DialFailure) -> Self {
        match failure {
            DialFailure::Uds { error, .. } => Self::Io(error),
            DialFailure::Remote(error) => Self::from(error),
            DialFailure::Closed(error) => closed(error),
        }
    }
}

impl From<LinkError> for SessionError {
    fn from(error: LinkError) -> Self {
        match error {
            LinkError::Read(error) | LinkError::Write(error) => Self::Io(error),
            LinkError::Framing(violation) => Self::Framing(violation),
            LinkError::Ws(error) => Self::from(error),
            LinkError::Closed(error) => closed(error),
            // A peer that stopped taking bytes is gone for every purpose a
            // caller has; the reconnect path is the remedy.
            LinkError::Stalled => {
                tracing::info!("a send made no progress; treating it as a disconnect");
                Self::Disconnected
            }
        }
    }
}

/// A QUIC close is a disconnection, unless its reason is a refusal no
/// retry can satisfy, which the caller must see to re-pair.
fn closed(error: DialError) -> SessionError {
    if is_fatal_refusal(&error) {
        SessionError::from(error)
    } else {
        SessionError::Disconnected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A caller reconnects on `Disconnected` and nothing else, so a stalled
    /// lane must land there, and faults worth reporting must not.
    #[test]
    fn a_stalled_lane_maps_to_disconnected_and_nothing_else_does() {
        let from = SessionError::from;
        assert!(matches!(
            from(DialError::Stalled("no pong".to_owned())),
            SessionError::Disconnected
        ));
        assert!(matches!(
            from(DialError::Unreachable("no route".to_owned())),
            SessionError::Unreachable(_)
        ));
        assert!(matches!(
            from(DialError::Connect("pin mismatch".to_owned())),
            SessionError::Connect(_)
        ));
        assert!(matches!(
            from(DialError::AuthRefused("unauthorized".to_owned())),
            SessionError::Connect(detail) if detail.contains(TOKEN_REFUSED)
        ));
        assert!(matches!(
            from(DialError::Io(io::ErrorKind::BrokenPipe.into())),
            SessionError::Io(_)
        ));
        assert!(matches!(
            SessionError::from(LinkError::Stalled),
            SessionError::Disconnected
        ));
    }

    /// Refusals no retry can satisfy stay fatal after flattening.
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
                DialError::Io(io::ErrorKind::ConnectionRefused.into()),
                false,
            ),
        ];
        for (error, fatal) in cases {
            let rendered = error.to_string();
            assert_eq!(
                SessionError::from(error).is_fatal_refusal(),
                fatal,
                "{rendered}"
            );
        }
        assert!(!SessionError::Refused("no such session".to_owned()).is_fatal_refusal());
    }

    /// A QUIC close is a disconnect unless it is a refusal to re-pair over.
    #[test]
    fn a_quic_close_is_a_disconnect_unless_the_token_was_refused() {
        assert!(matches!(
            SessionError::from(LinkError::Closed(DialError::Connect("closed".to_owned()))),
            SessionError::Disconnected
        ));
        let refused = SessionError::from(LinkError::Closed(DialError::AuthRefused(
            "unauthorized".to_owned(),
        )));
        assert!(refused.is_fatal_refusal(), "{refused}");
    }

    /// The OS error kind survives every path a gone peer is classified on.
    #[test]
    fn io_kinds_survive_from_dial_read_and_write() {
        let kind = |error: SessionError| match error {
            SessionError::Io(error) => error.kind(),
            other => panic!("expected Io, got {other:?}"),
        };
        let uds = DialFailure::Uds {
            path: "/nowhere".into(),
            error: io::ErrorKind::NotFound.into(),
        };
        assert_eq!(kind(uds.into()), io::ErrorKind::NotFound);
        let read = LinkError::Read(io::ErrorKind::ConnectionReset.into());
        assert_eq!(kind(read.into()), io::ErrorKind::ConnectionReset);
        let write = LinkError::Write(io::ErrorKind::BrokenPipe.into());
        assert_eq!(kind(write.into()), io::ErrorKind::BrokenPipe);
    }
}
